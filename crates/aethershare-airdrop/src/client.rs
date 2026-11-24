//! AirDrop Client (`/Discover` -> `/Ask` -> `/Upload`) for sending files to
//! Apple devices and AirDrop-compatible Android devices.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use log::info;
use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::archive::create_cpio_gz;
use crate::plist_codec::{
    AirDropFileEntry, AskRequest, AskResponse, DiscoverRequest, DiscoverResponse, decode_plist,
    encode_binary_plist,
};

/// Custom TLS verifier for AirDrop: Apple and Quick Share AirDrop peers use
/// self-signed device certificates over mDNS / link-local channels.
#[derive(Debug)]
struct AirDropCertVerifier;

impl ServerCertVerifier for AirDropCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

pub struct AirDropClient {
    sender_name: String,
    use_tls: bool,
}

impl AirDropClient {
    pub fn new(sender_name: String, use_tls: bool) -> Self {
        Self {
            sender_name,
            use_tls,
        }
    }

    pub async fn send_files(
        &self,
        addr: &str,
        files: &[PathBuf],
    ) -> Result<DiscoverResponse, anyhow::Error> {
        if files.is_empty() {
            return Err(anyhow!("No files specified for AirDrop transfer"));
        }

        let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await??;

        if self.use_tls {
            let mut config = ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AirDropCertVerifier))
                .with_no_client_auth();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            let connector = TlsConnector::from(Arc::new(config));
            let domain = ServerName::try_from("airdrop.local").unwrap();
            let mut tls_stream = connector.connect(domain, tcp).await?;
            self.drive_transfer(&mut tls_stream, files).await
        } else {
            let mut plain_stream = tcp;
            self.drive_transfer(&mut plain_stream, files).await
        }
    }

    async fn drive_transfer<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        stream: &mut S,
        files: &[PathBuf],
    ) -> Result<DiscoverResponse, anyhow::Error> {
        // 1. POST /Discover
        let disc_req = encode_binary_plist(&DiscoverRequest {
            sender_record_data: None,
        })?;
        let (status, disc_body) =
            http_post(stream, "/Discover", "application/octet-stream", &disc_req).await?;
        if status != 200 {
            return Err(anyhow!("AirDrop /Discover failed with HTTP {status}"));
        }
        let disc_resp: DiscoverResponse = decode_plist(&disc_body)?;
        info!(
            "AirDropClient: discovered receiver '{}' ({})",
            disc_resp.receiver_computer_name, disc_resp.receiver_model_name
        );

        // 2. POST /Ask
        let file_entries: Vec<AirDropFileEntry> = files
            .iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().into_owned();
                Some(AirDropFileEntry {
                    file_bom_path: Some(format!("./{name}")),
                    file_name: name,
                    file_type: Some("public.data".to_string()),
                    file_is_directory: Some(false),
                    convert_media_formats: Some(false),
                })
            })
            .collect();

        let ask_req = encode_binary_plist(&AskRequest {
            sender_computer_name: Some(self.sender_name.clone()),
            sender_model_name: Some("LinuxDesktop1,1".to_string()),
            sender_id: Some("AETHERSHARE01".to_string()),
            bundle_id: Some("com.apple.finder".to_string()),
            convert_media_formats: Some(false),
            files: file_entries,
            items: Vec::new(),
        })?;

        info!("AirDropClient: waiting for receiver consent (/Ask)...");
        let (ask_status, ask_body) =
            http_post(stream, "/Ask", "application/octet-stream", &ask_req).await?;
        if ask_status != 200 {
            return Err(anyhow!(
                "AirDrop transfer rejected by receiver (HTTP {ask_status})"
            ));
        }
        let _ask_resp: Option<AskResponse> = decode_plist(&ask_body).ok();

        // 3. POST /Upload (cpio.gz stream)
        let archive = create_cpio_gz(files)?;
        info!(
            "AirDropClient: uploading {} bytes of cpio.gz payload (/Upload)...",
            archive.len()
        );
        let (up_status, _) = http_post(stream, "/Upload", "application/x-cpio", &archive).await?;
        if up_status != 200 {
            return Err(anyhow!("AirDrop /Upload failed with HTTP {up_status}"));
        }

        info!("AirDropClient: transfer completed successfully!");
        Ok(disc_resp)
    }
}

async fn http_post<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    path: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(u16, Vec<u8>), anyhow::Error> {
    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: AirDrop/1.0\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;

    // Read response headers
    let mut hdr_buf = Vec::with_capacity(512);
    let mut b = [0u8; 1];
    loop {
        if stream.read(&mut b).await? == 0 {
            return Err(anyhow!("EOF reading AirDrop HTTP response"));
        }
        hdr_buf.push(b[0]);
        if hdr_buf.len() >= 4 && &hdr_buf[hdr_buf.len() - 4..] == b"\r\n\r\n" {
            break;
        }
    }

    let hdr_str = String::from_utf8_lossy(&hdr_buf);
    let mut lines = hdr_str.lines();
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);

    let mut content_len = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_len = v.trim().parse().unwrap_or(0);
            }
        }
    }

    let mut resp_body = vec![0u8; content_len];
    if content_len > 0 {
        stream.read_exact(&mut resp_body).await?;
    }
    Ok((status, resp_body))
}
