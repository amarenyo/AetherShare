//! Built-in Material 3 Expressive Web UI & Local Desktop Bridge API for AetherShare.
//!
//! Powers both `aethershare ui` and the native Tauri v2 desktop app (`app/src-tauri`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aethershare_airdrop::{AirDropManager, AirDropRadioStatus};
use aethershare_core::channel::{ChannelMessage, Message, TransferAction};
use aethershare_core::net_filter::{diagnose_interfaces, filtered_ipv4_addrs};
use aethershare_core::{EndpointInfo, OutboundPayload, RQS, SendInfo, Visibility};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

const UI_HTML: &str = include_str!("../../../app/ui/dist/index.html");

#[derive(Serialize)]
struct StatusResponse {
    device_name: String,
    visibility: Visibility,
    qs_port: Option<u32>,
    airdrop_port: u16,
    active_lan_ips: Vec<String>,
    interfaces: Vec<aethershare_core::net_filter::InterfaceDiagnostic>,
    download_dir: String,
    auto_accept: bool,
    system_accent: Option<String>,
    system_color_scheme: Option<String>,
    system_lang: String,
    version: String,
    airdrop: AirDropRadioStatus,
}

#[derive(Deserialize)]
struct ActionRequest {
    id: String,
    action: String,
}

#[derive(Deserialize)]
struct VisibilityRequest {
    visibility: String,
}

#[derive(Deserialize)]
struct SendRequest {
    id: String,
    name: String,
    addr: String,
    ble: bool,
    protocol: Option<String>,
    files: Vec<String>,
}

#[derive(Deserialize)]
struct OpenPathRequest {
    path: String,
}

#[derive(Deserialize)]
struct SettingsPatch {
    auto_accept: Option<bool>,
    download_dir: Option<String>,
    device_name: Option<String>,
    language: Option<String>,
}

#[derive(Serialize)]
struct PickedFile {
    path: String,
    name: String,
    size: u64,
}

#[derive(Serialize)]
struct PickFilesResponse {
    files: Vec<PickedFile>,
}

#[derive(Serialize)]
struct PickFolderResponse {
    path: Option<String>,
}

fn default_download_dir() -> String {
    std::env::var("HOME")
        .map(|h| format!("{h}/Downloads"))
        .unwrap_or_else(|_| ".".to_string())
}

#[allow(clippy::too_many_arguments)]
pub async fn serve_web_ui(
    port: u16,
    device_name: String,
    airdrop_port: u16,
    rqs: Arc<Mutex<RQS>>,
    airdrop: AirDropManager,
    msg_tx: broadcast::Sender<ChannelMessage>,
    disc_tx: broadcast::Sender<EndpointInfo>,
    send_tx: mpsc::Sender<SendInfo>,
    vis_tx: Arc<Mutex<watch::Sender<Visibility>>>,
    auto_accept: Arc<AtomicBool>,
    ctk: CancellationToken,
) -> Result<(), anyhow::Error> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;

    loop {
        tokio::select! {
            _ = ctk.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue; };
                let device_name = device_name.clone();
                let rqs = rqs.clone();
                let airdrop = airdrop.clone();
                let msg_tx = msg_tx.clone();
                let disc_tx = disc_tx.clone();
                let send_tx = send_tx.clone();
                let vis_tx = vis_tx.clone();
                let auto_accept = auto_accept.clone();

                tokio::spawn(async move {
                    if let Err(e) = handle_web_conn(
                        stream,
                        device_name,
                        airdrop_port,
                        rqs,
                        airdrop,
                        msg_tx,
                        disc_tx,
                        send_tx,
                        vis_tx,
                        auto_accept,
                    )
                    .await
                    {
                        debug!("Web UI connection closed: {e}");
                    }
                });
            }
        }
    }
    Ok(())
}

async fn read_http_request(
    stream: &mut TcpStream,
) -> Result<Option<(String, String, String)>, anyhow::Error> {
    let mut raw: Vec<u8> = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let mut header_end: Option<usize> = None;
    let mut content_length: usize = 0;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(end) = header_end {
            let body_received = raw.len().saturating_sub(end);
            if body_received >= content_length {
                break;
            }
        }
        let n = match tokio::time::timeout_at(deadline, stream.read(&mut tmp)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&tmp[..n]);
        if raw.len() > 2 * 1024 * 1024 {
            break;
        }
        if header_end.is_none() {
            if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                let end = pos + 4;
                header_end = Some(end);
                let hdr_str = String::from_utf8_lossy(&raw[..pos]);
                for line in hdr_str.lines().skip(1) {
                    if let Some((k, v)) = line.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            content_length = v.trim().parse::<usize>().unwrap_or(0);
                        }
                    }
                }
            }
        }
    }

    if raw.is_empty() {
        return Ok(None);
    }
    let end = header_end.unwrap_or(raw.len());
    let hdr_str = String::from_utf8_lossy(&raw[..end]);
    let first_line = hdr_str.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let body_slice = if end < raw.len() {
        let take = if content_length > 0 {
            content_length.min(raw.len() - end)
        } else {
            raw.len() - end
        };
        &raw[end..end + take]
    } else {
        &[]
    };
    let body = String::from_utf8_lossy(body_slice).to_string();
    Ok(Some((method, path, body)))
}

#[allow(clippy::too_many_arguments)]
async fn handle_web_conn(
    mut stream: TcpStream,
    device_name: String,
    airdrop_port: u16,
    rqs: Arc<Mutex<RQS>>,
    airdrop: AirDropManager,
    msg_tx: broadcast::Sender<ChannelMessage>,
    disc_tx: broadcast::Sender<EndpointInfo>,
    send_tx: mpsc::Sender<SendInfo>,
    vis_tx: Arc<Mutex<watch::Sender<Visibility>>>,
    auto_accept: Arc<AtomicBool>,
) -> Result<(), anyhow::Error> {
    let Some((method, path, body_owned)) = read_http_request(&mut stream).await? else {
        return Ok(());
    };
    let method = method.as_str();
    let path = path.as_str();
    let body = body_owned.as_str();

    if method == "OPTIONS" {
        let hdr = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nConnection: close\r\n\r\n";
        stream.write_all(hdr.as_bytes()).await?;
        stream.flush().await?;
        return Ok(());
    }

    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => {
            write_response(&mut stream, 200, "text/html; charset=utf-8", UI_HTML.as_bytes()).await?;
        }
        ("GET", "/api/status") => {
            let (vis, qs_port, dl_dir, live_name) = {
                let guard = rqs.lock().unwrap();
                let v = *vis_tx.lock().unwrap().borrow();
                let dl = guard
                    .get_download_path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(default_download_dir);
                (v, guard.port_number, dl, guard.get_device_name())
            };
            let (system_accent, system_color_scheme) = read_system_theme().await;
            let ad_status = airdrop.status().await;
            let status = StatusResponse {
                device_name: if live_name.is_empty() {
                    device_name
                } else {
                    live_name
                },
                visibility: vis,
                qs_port,
                airdrop_port,
                active_lan_ips: filtered_ipv4_addrs()
                    .into_iter()
                    .map(|ip| ip.to_string())
                    .collect(),
                interfaces: diagnose_interfaces(),
                download_dir: dl_dir,
                auto_accept: auto_accept.load(Ordering::Relaxed),
                system_accent,
                system_color_scheme,
                system_lang: aethershare_core::i18n::detect_system_lang().code().to_string(),
                version: aethershare_core::APP_VERSION.to_string(),
                airdrop: ad_status,
            };
            let json = serde_json::to_vec(&status)?;
            write_response(&mut stream, 200, "application/json", &json).await?;
        }
        ("GET", "/api/events") => {
            let hdr = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\n\r\n";
            stream.write_all(hdr.as_bytes()).await?;
            stream.flush().await?;

            let mut m_rx = msg_tx.subscribe();
            let mut d_rx = disc_tx.subscribe();

            loop {
                tokio::select! {
                    Ok(cm) = m_rx.recv() => {
                        if let Ok(j) = serde_json::to_string(&cm) {
                            let pkt = format!("event: transfer\ndata: {j}\n\n");
                            if stream.write_all(pkt.as_bytes()).await.is_err() {
                                break;
                            }
                            let _ = stream.flush().await;
                        }
                    }
                    Ok(ei) = d_rx.recv() => {
                        if let Ok(j) = serde_json::to_string(&ei) {
                            let pkt = format!("event: endpoint\ndata: {j}\n\n");
                            if stream.write_all(pkt.as_bytes()).await.is_err() {
                                break;
                            }
                            let _ = stream.flush().await;
                        }
                    }
                }
            }
        }
        ("POST", "/api/discovery/start") => {
            info!("UI: start_discovery");
            if !airdrop.is_exclusive_active().await {
                let mut guard = rqs.lock().unwrap();
                let _ = guard.discovery(disc_tx.clone());
            }
            let _ = airdrop.start_discovery().await;
            write_response(&mut stream, 200, "application/json", br#"{"ok":true}"#).await?;
        }
        ("POST", "/api/discovery/stop") => {
            info!("UI: stop_discovery");
            rqs.lock().unwrap().stop_discovery();
            if !airdrop.is_exclusive_active().await {
                airdrop.stop_discovery().await;
            }
            write_response(&mut stream, 200, "application/json", br#"{"ok":true}"#).await?;
        }
        ("POST", "/api/airdrop/enable") => {
            rqs.lock().unwrap().stop_discovery();
            match airdrop.enable().await {
                Ok(st) => {
                    let body = serde_json::to_vec(&serde_json::json!({ "ok": true, "airdrop": st }))?;
                    write_response(&mut stream, 200, "application/json", &body).await?;
                }
                Err(e) => {
                    warn!("POST /api/airdrop/enable failed: {e}");
                    let st = airdrop.probe().await;
                    let body = serde_json::to_vec(&serde_json::json!({
                        "ok": false,
                        "error": e.to_string(),
                        "airdrop": st
                    }))?;
                    write_response(&mut stream, 200, "application/json", &body).await?;
                }
            }
        }
        ("POST", "/api/airdrop/disable") => {
            let st = airdrop.disable().await.unwrap_or_else(|_| AirDropRadioStatus::default());
            let body = serde_json::to_vec(&serde_json::json!({ "ok": true, "airdrop": st }))?;
            write_response(&mut stream, 200, "application/json", &body).await?;
        }
        ("POST", "/api/visibility") => {
            if let Ok(req) = serde_json::from_str::<VisibilityRequest>(body) {
                let nv = match req.visibility.as_str() {
                    "Invisible" => Visibility::Invisible,
                    "Temporarily" => Visibility::Temporarily,
                    _ => Visibility::Visible,
                };
                rqs.lock().unwrap().change_visibility(nv);
            }
            write_response(&mut stream, 200, "application/json", br#"{"ok":true}"#).await?;
        }
        ("POST", "/api/transfer/action") => {
            if let Ok(req) = serde_json::from_str::<ActionRequest>(body) {
                let action = match req.action.as_str() {
                    "Accept" => TransferAction::ConsentAccept,
                    "Decline" => TransferAction::ConsentDecline,
                    _ => TransferAction::TransferCancel,
                };
                let _ = msg_tx.send(ChannelMessage {
                    id: req.id,
                    msg: Message::Lib { action },
                });
            }
            write_response(&mut stream, 200, "application/json", br#"{"ok":true}"#).await?;
        }
        ("POST", "/api/send") => {
            match serde_json::from_str::<SendRequest>(body) {
                Ok(req) => {
                    let expanded = expand_file_paths(&req.files);
                    info!(
                        "POST /api/send: id={} name={} protocol={:?} files={}",
                        req.id,
                        req.name,
                        req.protocol,
                        expanded.len()
                    );
                    if expanded.is_empty() {
                        warn!("POST /api/send: no readable files from {:?}", req.files);
                        write_response(
                            &mut stream,
                            400,
                            "application/json",
                            br#"{"ok":false,"error":"No readable files selected"}"#,
                        )
                        .await?;
                    } else if req.protocol.as_deref() == Some("airdrop") || req.id.starts_with("airdrop://")
                    {
                        let paths: Vec<PathBuf> = expanded.iter().map(PathBuf::from).collect();
                        let ad = airdrop.clone();
                        let id = req.id.clone();
                        let peer_name = req.name.clone();
                        tokio::spawn(async move {
                            if let Err(e) = ad.send_files(id, peer_name, paths).await {
                                warn!("AirDrop send failed: {e}");
                            }
                        });
                        write_response(&mut stream, 200, "application/json", br#"{"ok":true}"#).await?;
                    } else {
                        let _ = send_tx
                            .send(SendInfo {
                                id: req.id,
                                name: req.name,
                                addr: req.addr,
                                ob: OutboundPayload::Files(expanded),
                                ble: req.ble,
                            })
                            .await;
                        write_response(&mut stream, 200, "application/json", br#"{"ok":true}"#).await?;
                    }
                }
                Err(e) => {
                    warn!("POST /api/send invalid JSON ({e}): {body}");
                    write_response(
                        &mut stream,
                        400,
                        "application/json",
                        br#"{"ok":false,"error":"Invalid send request"}"#,
                    )
                    .await?;
                }
            }
        }
        ("POST", "/api/pick-files") => {
            let paths = pick_files_dialog().await;
            let files: Vec<PickedFile> = paths
                .into_iter()
                .map(|p| {
                    let pb = PathBuf::from(&p);
                    let name = pb
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.clone());
                    let size = std::fs::metadata(&pb).map(|m| m.len()).unwrap_or(0);
                    PickedFile { path: p, name, size }
                })
                .collect();
            let json = serde_json::to_vec(&PickFilesResponse { files })?;
            write_response(&mut stream, 200, "application/json", &json).await?;
        }
        ("POST", "/api/pick-folder") => {
            let path = pick_folder_dialog().await;
            let json = serde_json::to_vec(&PickFolderResponse { path })?;
            write_response(&mut stream, 200, "application/json", &json).await?;
        }
        ("POST", "/api/open-path") => {
            let target = serde_json::from_str::<OpenPathRequest>(body)
                .ok()
                .map(|r| r.path)
                .filter(|p| !p.trim().is_empty())
                .unwrap_or_else(|| {
                    rqs.lock()
                        .unwrap()
                        .get_download_path()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(default_download_dir)
                });
            let _ = tokio::process::Command::new("xdg-open").arg(&target).spawn();
            write_response(&mut stream, 200, "application/json", br#"{"ok":true}"#).await?;
        }
        ("POST", "/api/settings") => {
            if let Ok(patch) = serde_json::from_str::<SettingsPatch>(body) {
                if let Some(aa) = patch.auto_accept {
                    auto_accept.store(aa, Ordering::Relaxed);
                }
                if let Some(dir) = patch.download_dir {
                    let pb = PathBuf::from(dir);
                    rqs.lock().unwrap().set_download_path(Some(pb));
                }
                if let Some(name) = patch.device_name {
                    if !name.trim().is_empty() {
                        rqs.lock().unwrap().set_device_name(name);
                    }
                }
                if let Some(lang) = patch.language {
                    let pref = aethershare_core::i18n::LangPref::from_str_lossy(&lang);
                    aethershare_core::i18n::set_language_pref(pref);
                }
            }
            write_response(&mut stream, 200, "application/json", br#"{"ok":true}"#).await?;
        }
        _ => {
            write_response(&mut stream, 404, "text/plain", b"Not Found").await?;
        }
    }
    Ok(())
}

async fn read_system_theme() -> (Option<String>, Option<String>) {
    let mut accent: Option<String> = None;
    let mut scheme: Option<String> = None;

    // 1. Standard XDG Desktop Portal (works on KDE Plasma 6, GNOME 47+, COSMIC, Hyprland)
    if let Ok(out) = tokio::process::Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            "Read",
            "ss",
            "org.freedesktop.appearance",
            "accent-color",
        ])
        .output()
        .await
    {
        if out.status.success() {
            let txt = String::from_utf8_lossy(&out.stdout);
            // Format: "v v (ddd) 0.321569 0.419608 0.572549"
            let nums: Vec<f64> = txt
                .split_whitespace()
                .filter_map(|tok| tok.parse::<f64>().ok())
                .collect();
            if nums.len() >= 3 {
                let r = (nums[0].clamp(0.0, 1.0) * 255.0).round() as u8;
                let g = (nums[1].clamp(0.0, 1.0) * 255.0).round() as u8;
                let b = (nums[2].clamp(0.0, 1.0) * 255.0).round() as u8;
                accent = Some(format!("#{r:02X}{g:02X}{b:02X}"));
            }
        }
    }

    if let Ok(out) = tokio::process::Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            "Read",
            "ss",
            "org.freedesktop.appearance",
            "color-scheme",
        ])
        .output()
        .await
    {
        if out.status.success() {
            let txt = String::from_utf8_lossy(&out.stdout);
            if let Some(code) = txt.split_whitespace().last().and_then(|s| s.parse::<u32>().ok()) {
                scheme = match code {
                    1 => Some("dark".to_string()),
                    2 => Some("light".to_string()),
                    _ => None,
                };
            }
        }
    }

    // 2. Fallback: KDE Plasma ~/.config/kdeglobals
    if accent.is_none() {
        if let Ok(home) = std::env::var("HOME") {
            if let Ok(contents) = std::fs::read_to_string(format!("{home}/.config/kdeglobals")) {
                let mut section = String::new();
                for line in contents.lines() {
                    let t = line.trim();
                    if t.starts_with('[') && t.ends_with(']') {
                        section = t.to_string();
                        continue;
                    }
                    let key_match = (section == "[General]" && t.starts_with("AccentColor="))
                        || (section == "[Colors:Selection]" && t.starts_with("BackgroundNormal="));
                    if key_match {
                        if let Some((_, val)) = t.split_once('=') {
                            let parts: Vec<u8> = val
                                .split(',')
                                .filter_map(|p| p.trim().parse::<u8>().ok())
                                .collect();
                            if parts.len() >= 3 {
                                accent = Some(format!("#{:02X}{:02X}{:02X}", parts[0], parts[1], parts[2]));
                                if section == "[General]" {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    (accent, scheme)
}

fn expand_file_paths(raw: &[String]) -> Vec<String> {
    fn visit(p: &std::path::Path, out: &mut Vec<String>) {
        if p.is_file() {
            out.push(p.to_string_lossy().to_string());
        } else if p.is_dir() {
            if let Ok(entries) = std::fs::read_dir(p) {
                for entry in entries.flatten() {
                    visit(&entry.path(), out);
                }
            }
        }
    }
    let mut out = Vec::new();
    for s in raw {
        visit(&PathBuf::from(s), &mut out);
    }
    out
}

async fn pick_files_dialog() -> Vec<String> {
    let start_dir = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    let title = aethershare_core::i18n::strings().dialog_pick_files;
    // Prefer KDE kdialog on Plasma, fall back to GTK zenity
    if let Ok(out) = tokio::process::Command::new("kdialog")
        .args([
            "--title",
            title,
            "--getopenfilename",
            &start_dir,
            "--multiple",
            "--separate-output",
        ])
        .output()
        .await
    {
        if out.status.success() {
            return String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
        }
        // User clicked Cancel in kdialog -> don't also pop up zenity
        if out.status.code() == Some(1) {
            return Vec::new();
        }
    }

    if let Ok(out) = tokio::process::Command::new("zenity")
        .args([
            "--file-selection",
            "--multiple",
            "--separator=\n",
            &format!("--title={title}"),
        ])
        .output()
        .await
    {
        if out.status.success() {
            return String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
        }
    }
    Vec::new()
}

async fn pick_folder_dialog() -> Option<String> {
    let start_dir = default_download_dir();
    let title = aethershare_core::i18n::strings().dialog_pick_folder;
    if let Ok(out) = tokio::process::Command::new("kdialog")
        .args([
            "--title",
            title,
            "--getexistingdirectory",
            &start_dir,
        ])
        .output()
        .await
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return (!s.is_empty()).then_some(s);
        }
        if out.status.code() == Some(1) {
            return None;
        }
    }

    if let Ok(out) = tokio::process::Command::new("zenity")
        .args([
            "--file-selection",
            "--directory",
            &format!("--title={title}"),
        ])
        .output()
        .await
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return (!s.is_empty()).then_some(s);
        }
    }
    None
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), anyhow::Error> {
    let hdr = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(hdr.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}
