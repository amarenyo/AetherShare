//! AirDrop HTTP/1.1 + TLS Server handling `/Discover`, `/Ask`, and `/Upload`.
//!
//! Integrates directly with `aethershare_core::channel::ChannelMessage` so AirDrop
//! consent requests and file transfers flow through the same UI and CLI handlers
//! as Android Quick Share transfers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aethershare_core::channel::{
    ChannelMessage, Message, MessageClient, TransferAction, TransferKind,
};
use aethershare_core::hdl::info::{TransferMetadata, TransferPayload, TransferPayloadKind};
use aethershare_core::utils::{DeviceType, RemoteDeviceInfo, get_download_dir};
use aethershare_core::TransferState;
use anyhow::anyhow;
use log::{debug, info, warn};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, broadcast};
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use crate::archive::extract_cpio_gz;
use crate::plist_codec::{
    AskRequest, AskResponse, DiscoverRequest, DiscoverResponse, decode_plist, encode_binary_plist,
};

#[derive(Debug, Clone)]
struct PendingAirDropTransfer {
    id: String,
    sender_name: String,
}

pub struct AirDropServer {
    listener: TcpListener,
    device_name: String,
    sender: broadcast::Sender<ChannelMessage>,
    auto_accept: bool,
    tls_acceptor: TlsAcceptor,
    pending: Arc<Mutex<HashMap<String, PendingAirDropTransfer>>>,
}

impl AirDropServer {
    pub async fn bind(
        port: u16,
        device_name: String,
        sender: broadcast::Sender<ChannelMessage>,
        auto_accept: bool,
    ) -> Result<Self, anyhow::Error> {
        let listener = TcpListener::bind(("0.0.0.0", port)).await?;
        let tls_acceptor = build_self_signed_tls_acceptor(&device_name)?;

        Ok(Self {
            listener,
            device_name,
            sender,
            auto_accept,
            tls_acceptor,
            pending: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn local_port(&self) -> Result<u16, anyhow::Error> {
        Ok(self.listener.local_addr()?.port())
    }

    pub async fn run(self, ctk: CancellationToken) -> Result<(), anyhow::Error> {
        let port = self.local_port()?;
        info!("AirDropServer: listening on 0.0.0.0:{port} (TLS + HTTP/1.1 /Discover /Ask /Upload)");

        loop {
            tokio::select! {
                _ = ctk.cancelled() => {
                    info!("AirDropServer: shutting down");
                    break;
                }
                accepted = self.listener.accept() => {
                    let Ok((stream, remote_addr)) = accepted else {
                        continue;
                    };
                    let peer_ip = remote_addr.ip().to_string();
                    let device_name = self.device_name.clone();
                    let sender = self.sender.clone();
                    let auto_accept = self.auto_accept;
                    let acceptor = self.tls_acceptor.clone();
                    let pending = self.pending.clone();

                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(
                            stream,
                            peer_ip,
                            device_name,
                            sender,
                            auto_accept,
                            acceptor,
                            pending,
                        )
                        .await
                        {
                            debug!("AirDropServer: connection ended: {e}");
                        }
                    });
                }
            }
        }
        Ok(())
    }
}

fn build_self_signed_tls_acceptor(device_name: &str) -> Result<TlsAcceptor, anyhow::Error> {
    let subject_alt_names = vec![
        device_name.to_string(),
        format!("{device_name}.local"),
        "localhost".to_string(),
    ];
    let certified = rcgen::generate_simple_self_signed(subject_alt_names)?;
    let cert_der = CertificateDer::from(certified.cert);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.key_pair.serialize_der(),
    ));

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

async fn handle_connection(
    stream: TcpStream,
    peer_ip: String,
    device_name: String,
    sender: broadcast::Sender<ChannelMessage>,
    auto_accept: bool,
    acceptor: TlsAcceptor,
    pending: Arc<Mutex<HashMap<String, PendingAirDropTransfer>>>,
) -> Result<(), anyhow::Error> {
    // Peek first byte: 0x16 = TLS Handshake ClientHello, otherwise plain HTTP/1.1
    let mut peek_byte = [0u8; 1];
    let n = stream.peek(&mut peek_byte).await?;
    if n == 0 {
        return Ok(());
    }

    if peek_byte[0] == 0x16 {
        let tls_stream = acceptor.accept(stream).await?;
        serve_http_stream(
            tls_stream,
            peer_ip,
            device_name,
            sender,
            auto_accept,
            pending,
        )
        .await
    } else {
        serve_http_stream(
            stream,
            peer_ip,
            device_name,
            sender,
            auto_accept,
            pending,
        )
        .await
    }
}

async fn serve_http_stream<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    peer_ip: String,
    device_name: String,
    sender: broadcast::Sender<ChannelMessage>,
    auto_accept: bool,
    pending: Arc<Mutex<HashMap<String, PendingAirDropTransfer>>>,
) -> Result<(), anyhow::Error> {
    // Support HTTP/1.1 keep-alive (AirDrop peers often send /Discover, /Ask, /Upload on the same TLS socket)
    loop {
        let Some((path, body)) = read_http_request(&mut stream).await? else {
            break;
        };
        info!("AirDropServer: request from {peer_ip}: POST {path} ({} bytes)", body.len());

        match path.as_str() {
            "/Discover" => {
                let _req: Option<DiscoverRequest> = decode_plist(&body).ok();
                let resp = DiscoverResponse::new(&device_name);
                let resp_bytes = encode_binary_plist(&resp)?;
                write_http_response(&mut stream, 200, "OK", "application/octet-stream", &resp_bytes)
                    .await?;
            }
            "/Ask" => {
                let ask: AskRequest = decode_plist(&body)?;
                let sender_name = ask
                    .sender_computer_name
                    .clone()
                    .unwrap_or_else(|| format!("AirDrop ({peer_ip})"));
                let file_names: Vec<String> = if !ask.files.is_empty() {
                    ask.files.iter().map(|f| f.file_name.clone()).collect()
                } else {
                    ask.items.clone()
                };

                let transfer_id = format!("airdrop-{peer_ip}");
                let mut rx = sender.subscribe();

                let metadata = TransferMetadata {
                    id: transfer_id.clone(),
                    source: Some(RemoteDeviceInfo {
                        name: format!("{sender_name} (AirDrop)"),
                        device_type: DeviceType::Phone,
                    }),
                    pin_code: None,
                    payload_kind: TransferPayloadKind::Files,
                    payload_preview: None,
                    payload: Some(TransferPayload::Files(file_names.clone())),
                    destination: Some(get_download_dir().to_string_lossy().into_owned()),
                    saved_paths: None,
                    total_bytes: 0,
                    ack_bytes: 0,
                };

                let _ = sender.send(ChannelMessage {
                    id: transfer_id.clone(),
                    msg: Message::Client(MessageClient {
                        kind: TransferKind::Inbound,
                        state: Some(TransferState::WaitingForUserConsent),
                        metadata: Some(metadata.clone()),
                    }),
                });

                let accepted = if auto_accept {
                    info!("AirDropServer: auto-accepting transfer from {sender_name}");
                    true
                } else {
                    wait_for_user_consent(&mut rx, &transfer_id).await
                };

                if accepted {
                    pending.lock().await.insert(
                        peer_ip.clone(),
                        PendingAirDropTransfer {
                            id: transfer_id.clone(),
                            sender_name,
                        },
                    );
                    let _ = sender.send(ChannelMessage {
                        id: transfer_id,
                        msg: Message::Client(MessageClient {
                            kind: TransferKind::Inbound,
                            state: Some(TransferState::ReceivingFiles),
                            metadata: Some(metadata),
                        }),
                    });
                    let resp = AskResponse::new(&device_name);
                    let resp_bytes = encode_binary_plist(&resp)?;
                    write_http_response(
                        &mut stream,
                        200,
                        "OK",
                        "application/octet-stream",
                        &resp_bytes,
                    )
                    .await?;
                } else {
                    let _ = sender.send(ChannelMessage {
                        id: transfer_id,
                        msg: Message::Client(MessageClient {
                            kind: TransferKind::Inbound,
                            state: Some(TransferState::Rejected),
                            metadata: Some(metadata),
                        }),
                    });
                    write_http_response(&mut stream, 401, "Unauthorized", "text/plain", b"Declined")
                        .await?;
                }
            }
            "/Upload" => {
                let dest_dir = get_download_dir();
                let extracted = extract_cpio_gz(&body, &dest_dir)?;
                let saved_paths: Vec<String> = extracted
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                let file_names: Vec<String> = extracted
                    .iter()
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect();

                let info_entry = pending.lock().await.remove(&peer_ip);
                let (transfer_id, sender_name) = match info_entry {
                    Some(p) => (p.id, p.sender_name),
                    None => (format!("airdrop-{peer_ip}"), format!("AirDrop ({peer_ip})")),
                };

                info!(
                    "AirDropServer: received {} file(s) from {sender_name} into {:?}",
                    saved_paths.len(),
                    dest_dir
                );

                let metadata = TransferMetadata {
                    id: transfer_id.clone(),
                    source: Some(RemoteDeviceInfo {
                        name: format!("{sender_name} (AirDrop)"),
                        device_type: DeviceType::Phone,
                    }),
                    pin_code: None,
                    payload_kind: TransferPayloadKind::Files,
                    payload_preview: None,
                    payload: Some(TransferPayload::Files(file_names)),
                    destination: Some(dest_dir.to_string_lossy().into_owned()),
                    saved_paths: Some(saved_paths),
                    total_bytes: body.len() as u64,
                    ack_bytes: body.len() as u64,
                };

                let _ = sender.send(ChannelMessage {
                    id: transfer_id,
                    msg: Message::Client(MessageClient {
                        kind: TransferKind::Inbound,
                        state: Some(TransferState::Finished),
                        metadata: Some(metadata),
                    }),
                });

                write_http_response(&mut stream, 200, "OK", "text/plain", b"OK").await?;
            }
            _ => {
                warn!("AirDropServer: unknown endpoint {path}");
                write_http_response(&mut stream, 404, "Not Found", "text/plain", b"Not Found")
                    .await?;
            }
        }
    }
    Ok(())
}

async fn wait_for_user_consent(
    rx: &mut broadcast::Receiver<ChannelMessage>,
    transfer_id: &str,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return false,
            msg = rx.recv() => {
                let Ok(cm) = msg else { return false; };
                if cm.id != transfer_id {
                    continue;
                }
                if let Message::Lib { action } = cm.msg {
                    return matches!(action, TransferAction::ConsentAccept);
                }
            }
        }
    }
}

/// Minimal HTTP/1.1 request parser supporting both `Content-Length` and `Transfer-Encoding: chunked`,
/// plus automatic `Expect: 100-continue` acknowledgement (required by Apple's `sharingd`).
async fn read_http_request<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
) -> Result<Option<(String, Vec<u8>)>, anyhow::Error> {
    let mut header_buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];

    let header_end = loop {
        match stream.read(&mut byte).await? {
            0 if header_buf.is_empty() => return Ok(None),
            0 => return Err(anyhow!("Unexpected EOF reading HTTP headers")),
            _ => {
                header_buf.push(byte[0]);
                if header_buf.len() >= 4
                    && &header_buf[header_buf.len() - 4..] == b"\r\n\r\n"
                {
                    break header_buf.len();
                }
                if header_buf.len() > 32 * 1024 {
                    return Err(anyhow!("HTTP headers too large"));
                }
            }
        }
    };

    let header_str = String::from_utf8_lossy(&header_buf[..header_end]);
    let mut lines = header_str.lines();
    let request_line = lines.next().ok_or_else(|| anyhow!("Empty request line"))?;
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/").to_string();

    let mut content_length: Option<usize> = None;
    let mut is_chunked = false;
    let mut expect_continue = false;

    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == "content-length" {
                content_length = v.parse().ok();
            } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                is_chunked = true;
            } else if k == "expect" && v.to_ascii_lowercase().contains("100-continue") {
                expect_continue = true;
            }
        }
    }

    if expect_continue {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
        stream.flush().await?;
    }

    let body = if is_chunked {
        read_chunked_body(stream).await?
    } else if let Some(len) = content_length {
        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await?;
        buf
    } else {
        Vec::new()
    };

    Ok(Some((path, body)))
}

async fn read_chunked_body<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, anyhow::Error> {
    let mut out = Vec::new();
    loop {
        let line = read_crlf_line(stream).await?;
        let size_hex = line.split(';').next().unwrap_or("").trim();
        let chunk_size = usize::from_str_radix(size_hex, 16)?;
        if chunk_size == 0 {
            let _ = read_crlf_line(stream).await;
            break;
        }
        let start = out.len();
        out.resize(start + chunk_size, 0);
        stream.read_exact(&mut out[start..]).await?;
        let mut crlf = [0u8; 2];
        stream.read_exact(&mut crlf).await?;
    }
    Ok(out)
}

async fn read_crlf_line<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String, anyhow::Error> {
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        if stream.read(&mut b).await? == 0 {
            return Err(anyhow!("EOF in chunked line"));
        }
        line.push(b[0]);
        if line.len() >= 2 && &line[line.len() - 2..] == b"\r\n" {
            line.truncate(line.len() - 2);
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
    }
}

async fn write_http_response<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), anyhow::Error> {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}
