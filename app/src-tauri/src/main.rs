//! AetherShare Desktop — Native Tauri v2 App (Material 3 Expressive)
//!
//! Runs the full Quick Share (mDNS + BLE 0xFEF3 + L2CAP CoC + GATT Weave + Wi-Fi Direct/Hotspot BWU)
//! and AirDrop engines directly in-process, with native Linux System Tray, OS Drag & Drop,
//! and Desktop Notifications.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aethershare_airdrop::{AirDropManager, AirDropRadioStatus};
use aethershare_core::channel::{ChannelMessage, Message, TransferAction};
use aethershare_core::net_filter::{diagnose_interfaces, filtered_ipv4_addrs};
use aethershare_core::{EndpointInfo, OutboundPayload, RQS, SendInfo, Visibility};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{DragDropEvent, Manager, WindowEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

const BRIDGE_PORT: u16 = 8787;
const AIRDROP_PORT: u16 = aethershare_airdrop::awdl::LOCAL_AIRDROP_PORT;

struct EngineHandle {
    rqs: Arc<Mutex<RQS>>,
    airdrop: AirDropManager,
    rt: Arc<tokio::runtime::Runtime>,
    ctk: CancellationToken,
}

fn default_download_dir() -> String {
    std::env::var("HOME")
        .map(|h| format!("{h}/Downloads"))
        .unwrap_or_else(|_| ".".to_string())
}

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        unsafe {
            std::env::set_var("RUST_LOG", "info");
        }
    }
    env_logger::Builder::from_default_env()
        .format_timestamp_secs()
        .init();

    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to initialize Tokio runtime"),
    );

    let device_name =
        sys_metrics::host::get_hostname().unwrap_or_else(|_| "AetherShare".to_string());
    let ctk = CancellationToken::new();
    let auto_accept = Arc::new(AtomicBool::new(false));

    // Start the full AetherShare Quick Share + AWDL AirDrop + Local Bridge Engine
    let (rqs_shared, airdrop_mgr): (Arc<Mutex<RQS>>, AirDropManager) = rt.block_on(async {
        let mut rqs = RQS::new(Visibility::Visible, None, None, Some(device_name.clone()));
        let (send_tx, _ble_rx) = rqs.run().await.expect("Failed to start AetherShare core");
        let msg_tx = rqs.message_sender.clone();
        let vis_tx = rqs.visibility_sender.clone();

        // Interactive desktop notifications ([Accept]/[Decline], native KDE JobViewV2 progress bar, [Open])
        aethershare_core::notifications::spawn_transfer_notifications(
            msg_tx.clone(),
            auto_accept.clone(),
        );

        let (disc_tx, _) = broadcast::channel::<EndpointInfo>(64);
        let airdrop = AirDropManager::new(
            device_name.clone(),
            auto_accept.clone(),
            msg_tx.clone(),
            disc_tx.clone(),
        );

        // Probe AWDL radio daemon (`aethershare-awdld` / `owlyd`).
        // In Shared mode (Wi-Fi stays connected), auto-enable AirDrop seamlessly.
        // In Exclusive mode (e.g. Intel AX201), wait for the user to toggle AirDrop Mode.
        let ad_startup = airdrop.clone();
        tokio::spawn(async move {
            let st = ad_startup.probe().await;
            info!(
                "AirDrop AWDL probe: available={}, mode={}, driver={:?}, device={:?}",
                st.available, st.mode, st.driver, st.device
            );
            if st.available && st.mode == "shared" {
                if let Err(e) = ad_startup.enable().await {
                    warn!("AirDrop auto-enable in shared mode failed: {e}");
                }
            }
        });

        let rqs_arc = Arc::new(Mutex::new(rqs));
        let rqs_bridge = rqs_arc.clone();
        let ad_bridge = airdrop.clone();
        let bridge_ctk = ctk.clone();
        let bridge_name = device_name.clone();
        let aa_bridge = auto_accept.clone();

        tokio::spawn(async move {
            if let Err(e) = serve_bridge(
                BRIDGE_PORT,
                bridge_name,
                AIRDROP_PORT,
                rqs_bridge,
                ad_bridge,
                msg_tx,
                disc_tx,
                send_tx,
                vis_tx,
                aa_bridge,
                bridge_ctk,
            )
            .await
            {
                error!("Desktop bridge error: {e}");
            }
        });

        (rqs_arc, airdrop)
    });

    let engine = EngineHandle {
        rqs: rqs_shared,
        airdrop: airdrop_mgr,
        rt: rt.clone(),
        ctk,
    };

    tauri::Builder::default()
        .manage(engine)
        .setup(|app| {
            let s = aethershare_core::i18n::strings();
            let show_i = MenuItem::with_id(app, "show", s.tray_open, true, None::<&str>)?;
            let vis_all =
                MenuItem::with_id(app, "vis_all", s.tray_visible_all, true, None::<&str>)?;
            let vis_off = MenuItem::with_id(app, "vis_off", s.tray_invisible, true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", s.tray_quit, true, None::<&str>)?;

            let menu = Menu::with_items(app, &[&show_i, &vis_all, &vis_off, &quit_i])?;

            let mut tray_builder = TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("AetherShare — Quick Share & AirDrop")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "vis_all" => {
                        let eng = app.state::<EngineHandle>();
                        eng.rqs.lock().unwrap().change_visibility(Visibility::Visible);
                    }
                    "vis_off" => {
                        let eng = app.state::<EngineHandle>();
                        eng.rqs.lock().unwrap().change_visibility(Visibility::Invisible);
                    }
                    "quit" => {
                        let eng = app.state::<EngineHandle>();
                        let ad = eng.airdrop.clone();
                        let _ = eng.rt.block_on(ad.disable());
                        eng.ctk.cancel();
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        if let Some(w) = tray.app_handle().get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                });

            if let Some(icon) = app.default_window_icon().cloned() {
                tray_builder = tray_builder.icon(icon);
            }
            let _ = tray_builder.build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Keep running in the system tray when closing the window
            WindowEvent::CloseRequested { api, .. } => {
                let _ = window.hide();
                api.prevent_close();
            }
            // Forward native OS Drag & Drop paths directly to the M3 Expressive UI
            WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) => {
                let str_paths: Vec<String> = paths
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                if let Ok(json) = serde_json::to_string(&str_paths) {
                    let js = format!(
                        "window.dispatchEvent(new CustomEvent('aethershare:drop-files', {{ detail: {{ paths: {json} }} }}));"
                    );
                    if let Some(wv) = window.app_handle().get_webview_window(window.label()) {
                        let _ = wv.eval(&js);
                    }
                }
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("Error while running AetherShare Desktop");
}

// ---- Local Bridge API (identical contract to aethershare-cli/src/web_ui.rs) ----

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

#[allow(clippy::too_many_arguments)]
async fn serve_bridge(
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
    info!("AetherShare Desktop Bridge listening on http://127.0.0.1:{port}");

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
                    if let Err(e) = handle_conn(
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
                        debug!("Bridge conn closed: {e}");
                    }
                });
            }
        }
    }
    Ok(())
}

async fn read_http_request(stream: &mut TcpStream) -> Result<Option<(String, String, String)>, anyhow::Error> {
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
async fn handle_conn(
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
                device_name: if live_name.is_empty() { device_name } else { live_name },
                visibility: vis,
                qs_port,
                airdrop_port,
                active_lan_ips: filtered_ipv4_addrs().into_iter().map(|ip| ip.to_string()).collect(),
                interfaces: diagnose_interfaces(),
                download_dir: dl_dir,
                auto_accept: auto_accept.load(Ordering::Relaxed),
                system_accent,
                system_color_scheme,
                system_lang: aethershare_core::i18n::detect_system_lang().code().to_string(),
                version: aethershare_core::APP_VERSION.to_string(),
                airdrop: ad_status,
            };
            write_json(&mut stream, &serde_json::to_vec(&status)?).await?;
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
                            if stream.write_all(pkt.as_bytes()).await.is_err() { break; }
                            let _ = stream.flush().await;
                        }
                    }
                    Ok(ei) = d_rx.recv() => {
                        if let Ok(j) = serde_json::to_string(&ei) {
                            let pkt = format!("event: endpoint\ndata: {j}\n\n");
                            if stream.write_all(pkt.as_bytes()).await.is_err() { break; }
                            let _ = stream.flush().await;
                        }
                    }
                }
            }
        }
        ("POST", "/api/discovery/start") => {
            {
                let mut guard = rqs.lock().unwrap();
                let _ = guard.discovery(disc_tx.clone());
            }
            let _ = airdrop.start_discovery().await;
            write_json(&mut stream, br#"{"ok":true}"#).await?;
        }
        ("POST", "/api/discovery/stop") => {
            rqs.lock().unwrap().stop_discovery();
            airdrop.stop_discovery().await;
            write_json(&mut stream, br#"{"ok":true}"#).await?;
        }
        ("POST", "/api/airdrop/enable") => {
            match airdrop.enable().await {
                Ok(st) => {
                    let body = serde_json::to_vec(&serde_json::json!({ "ok": true, "airdrop": st }))?;
                    write_json(&mut stream, &body).await?;
                }
                Err(e) => {
                    warn!("POST /api/airdrop/enable failed: {e}");
                    let st = airdrop.probe().await;
                    let body = serde_json::to_vec(&serde_json::json!({
                        "ok": false,
                        "error": e.to_string(),
                        "airdrop": st
                    }))?;
                    write_json(&mut stream, &body).await?;
                }
            }
        }
        ("POST", "/api/airdrop/disable") => {
            let st = airdrop.disable().await.unwrap_or_else(|_| AirDropRadioStatus::default());
            let body = serde_json::to_vec(&serde_json::json!({ "ok": true, "airdrop": st }))?;
            write_json(&mut stream, &body).await?;
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
            write_json(&mut stream, br#"{"ok":true}"#).await?;
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
            write_json(&mut stream, br#"{"ok":true}"#).await?;
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
                        write_status_json(&mut stream, 400, br#"{"ok":false,"error":"No readable files selected"}"#).await?;
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
                        write_json(&mut stream, br#"{"ok":true}"#).await?;
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
                        write_json(&mut stream, br#"{"ok":true}"#).await?;
                    }
                }
                Err(e) => {
                    warn!("POST /api/send invalid JSON ({e}): {body}");
                    write_status_json(&mut stream, 400, br#"{"ok":false,"error":"Invalid send request"}"#).await?;
                }
            }
        }
        ("POST", "/api/pick-files") => {
            let paths = pick_files_dialog().await;
            let files: Vec<PickedFile> = paths
                .into_iter()
                .map(|p| {
                    let pb = PathBuf::from(&p);
                    let name = pb.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.clone());
                    let size = std::fs::metadata(&pb).map(|m| m.len()).unwrap_or(0);
                    PickedFile { path: p, name, size }
                })
                .collect();
            write_json(&mut stream, &serde_json::to_vec(&PickFilesResponse { files })?).await?;
        }
        ("POST", "/api/pick-folder") => {
            let path = pick_folder_dialog().await;
            write_json(&mut stream, &serde_json::to_vec(&PickFolderResponse { path })?).await?;
        }
        ("POST", "/api/open-path") => {
            let target = serde_json::from_str::<OpenPathRequest>(body)
                .ok()
                .map(|r| r.path)
                .filter(|p| !p.trim().is_empty())
                .unwrap_or_else(default_download_dir);
            let _ = tokio::process::Command::new("xdg-open").arg(&target).spawn();
            write_json(&mut stream, br#"{"ok":true}"#).await?;
        }
        ("POST", "/api/settings") => {
            if let Ok(patch) = serde_json::from_str::<SettingsPatch>(body) {
                if let Some(aa) = patch.auto_accept {
                    auto_accept.store(aa, Ordering::Relaxed);
                }
                if let Some(dir) = patch.download_dir {
                    rqs.lock().unwrap().set_download_path(Some(PathBuf::from(dir)));
                }
                if let Some(name) = patch.device_name {
                    if !name.trim().is_empty() {
                        rqs.lock().unwrap().set_device_name(name.clone());
                        airdrop.set_device_name(name).await;
                    }
                }
                if let Some(lang) = patch.language {
                    let pref = aethershare_core::i18n::LangPref::from_str_lossy(&lang);
                    aethershare_core::i18n::set_language_pref(pref);
                }
            }
            write_json(&mut stream, br#"{"ok":true}"#).await?;
        }
        _ => {
            let hdr = "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\nNot Found";
            stream.write_all(hdr.as_bytes()).await?;
        }
    }
    Ok(())
}

async fn read_system_theme() -> (Option<String>, Option<String>) {
    let mut accent: Option<String> = None;
    let mut scheme: Option<String> = None;

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
        .args(["--title", title, "--getexistingdirectory", &start_dir])
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

async fn write_json(stream: &mut TcpStream, body: &[u8]) -> Result<(), anyhow::Error> {
    write_status_json(stream, 200, body).await
}

async fn write_status_json(
    stream: &mut TcpStream,
    code: u16,
    body: &[u8],
) -> Result<(), anyhow::Error> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        _ => "Error",
    };
    let hdr = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(hdr.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}
