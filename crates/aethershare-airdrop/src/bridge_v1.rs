//! Shared `/api/v1` HTTP & SSE bridge router for AetherShare Desktop and CLI.
//!
//! Exposes a versioned, token-authenticated local HTTP/SSE API designed for
//! external surfaces such as KDE Plasma widgets / Dynamic Islands, scripts,
//! and companion frontends.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aethershare_core::api_v1::{ApiV1State, ResolvedSendRoute, api_error_json};
use aethershare_core::channel::{ChannelMessage, Message, TransferAction};
use aethershare_core::{EndpointInfo, OutboundPayload, RQS, SendInfo, Visibility};
use log::{info, warn};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::AirDropManager;

/// Parsed HTTP/1.1 request with full `Content-Length` body.
#[derive(Debug, Clone)]
pub struct ParsedHttpRequest {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub auth_header: Option<String>,
    pub body: String,
}

impl ParsedHttpRequest {
    pub fn query_param(&self, key: &str) -> Option<String> {
        let q = self.query.as_deref()?;
        for pair in q.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == key {
                    return Some(percent_decode(v));
                }
            }
        }
        None
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex_str) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(val) = u8::from_str_radix(hex_str, 16) {
                    out.push(val);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Reads an entire HTTP/1.1 request (headers + full `Content-Length` body).
pub async fn read_http_request(
    stream: &mut TcpStream,
) -> Result<Option<ParsedHttpRequest>, anyhow::Error> {
    let mut raw: Vec<u8> = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let mut header_end: Option<usize> = None;
    let mut content_length: usize = 0;
    let mut auth_header: Option<String> = None;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
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
                        let key = k.trim();
                        if key.eq_ignore_ascii_case("content-length") {
                            content_length = v.trim().parse::<usize>().unwrap_or(0);
                        } else if key.eq_ignore_ascii_case("authorization") {
                            auth_header = Some(v.trim().to_string());
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
    let target = parts.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (target.to_string(), None),
    };
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

    Ok(Some(ParsedHttpRequest {
        method,
        path,
        query,
        auth_header,
        body,
    }))
}

pub async fn write_json_status(
    stream: &mut TcpStream,
    code: u16,
    body: &[u8],
) -> Result<(), anyhow::Error> {
    let reason = match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let hdr = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, DELETE, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Authorization\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(hdr.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

/// Recursively expands directories into individual readable files.
pub fn expand_file_paths(raw: &[String]) -> Vec<String> {
    fn visit(path: &Path, out: &mut Vec<String>) {
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.is_file() {
                out.push(path.to_string_lossy().to_string());
            } else if meta.is_dir() {
                if let Ok(entries) = std::fs::read_dir(path) {
                    let mut children: Vec<PathBuf> =
                        entries.flatten().map(|e| e.path()).collect();
                    children.sort();
                    for child in children {
                        visit(&child, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for s in raw {
        visit(Path::new(s), &mut out);
    }
    out
}

/// Spawns a background reaper that prunes expired discovery sessions and stale
/// unrefreshed devices every 5s, stopping hardware scanning if the last session
/// expires (e.g. crashed widget).
pub fn spawn_session_reaper(
    api_v1: ApiV1State,
    rqs: Arc<Mutex<RQS>>,
    airdrop: AirDropManager,
    ctk: CancellationToken,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = ctk.cancelled() => break,
                _ = tick.tick() => {
                    api_v1.devices.prune_stale();
                    let (should_stop, _) = api_v1.discovery.prune_expired();
                    if should_stop {
                        info!("api_v1: all discovery sessions expired; stopping hardware scan");
                        rqs.lock().unwrap().stop_discovery();
                        if !airdrop.is_exclusive_active().await {
                            airdrop.stop_discovery().await;
                        }
                    }
                }
            }
        }
    });
}

#[derive(Deserialize, Default)]
struct CreateDiscoverySessionReq {
    client: Option<String>,
    ttl_seconds: Option<u64>,
}

#[derive(Deserialize, Default)]
struct HeartbeatReq {
    ttl_seconds: Option<u64>,
}

#[derive(Deserialize)]
struct CreateTransferReq {
    request_id: Option<String>,
    device_id: String,
    files: Vec<String>,
}

#[derive(Deserialize)]
struct VisibilityReq {
    visibility: String,
}

/// Context passed into [`handle_v1_request`].
pub struct V1Context<'a> {
    pub device_name: &'a str,
    pub airdrop_port: u16,
    pub rqs: &'a Arc<Mutex<RQS>>,
    pub airdrop: &'a AirDropManager,
    pub api_v1: &'a ApiV1State,
    pub msg_tx: &'a broadcast::Sender<ChannelMessage>,
    pub disc_tx: &'a broadcast::Sender<EndpointInfo>,
    pub send_tx: &'a mpsc::Sender<SendInfo>,
    pub vis_tx: &'a Arc<Mutex<watch::Sender<Visibility>>>,
    pub auto_accept: &'a Arc<AtomicBool>,
}

/// Handles any request whose path starts with `/api/v1`.
pub async fn handle_v1_request(
    stream: &mut TcpStream,
    req: &ParsedHttpRequest,
    ctx: V1Context<'_>,
) -> Result<(), anyhow::Error> {
    let method = req.method.as_str();
    let path = req.path.as_str();

    // Unauthenticated lightweight health probe for widgets/scripts
    if method == "GET" && path == "/api/v1/health" {
        let body = serde_json::to_vec(&serde_json::json!({
            "ok": true,
            "api_version": "v1",
            "version": aethershare_core::APP_VERSION,
        }))?;
        return write_json_status(stream, 200, &body).await;
    }

    // All other /api/v1/* endpoints require the local access token
    let query_token = req.query_param("token");
    if !ctx
        .api_v1
        .auth
        .verify(req.auth_header.as_deref(), query_token.as_deref())
    {
        return write_json_status(
            stream,
            401,
            &api_error_json(
                "UNAUTHORIZED",
                "Missing or invalid local API access token. Pass 'Authorization: Bearer <token>' (or '?token=<token>' for SSE) from $XDG_RUNTIME_DIR/aethershare/api.token.",
            ),
        )
        .await;
    }

    // ---- GET /api/v1/status ----
    if method == "GET" && path == "/api/v1/status" {
        let (vis, qs_port, dl_dir, live_name) = {
            let guard = ctx.rqs.lock().unwrap();
            let v = *ctx.vis_tx.lock().unwrap().borrow();
            let dl = guard
                .get_download_path()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| {
                    std::env::var("HOME")
                        .map(|h| format!("{h}/Downloads"))
                        .unwrap_or_else(|_| ".".into())
                });
            (v, guard.port_number, dl, guard.get_device_name())
        };
        let ad = ctx.airdrop.status().await;
        let body = serde_json::to_vec(&serde_json::json!({
            "api_version": "v1",
            "version": aethershare_core::APP_VERSION,
            "device_name": if live_name.is_empty() { ctx.device_name } else { live_name.as_str() },
            "visibility": vis,
            "download_dir": dl_dir,
            "auto_accept": ctx.auto_accept.load(Ordering::Relaxed),
            "qs_port": qs_port,
            "airdrop_port": ctx.airdrop_port,
            "airdrop": ad,
            "active_discovery_sessions": ctx.api_v1.discovery.active_count(),
            "reachable_devices": ctx.api_v1.devices.list_reachable().len(),
        }))?;
        return write_json_status(stream, 200, &body).await;
    }

    // ---- GET /api/v1/devices ----
    if method == "GET" && path == "/api/v1/devices" {
        let devices = ctx.api_v1.devices.list_reachable();
        let body = serde_json::to_vec(&serde_json::json!({
            "devices": devices,
        }))?;
        return write_json_status(stream, 200, &body).await;
    }

    // ---- POST /api/v1/discovery/sessions ----
    if method == "POST" && path == "/api/v1/discovery/sessions" {
        let parsed: CreateDiscoverySessionReq = if req.body.trim().is_empty() {
            CreateDiscoverySessionReq::default()
        } else {
            match serde_json::from_str(&req.body) {
                Ok(v) => v,
                Err(e) => {
                    return write_json_status(
                        stream,
                        400,
                        &api_error_json("INVALID_REQUEST", format!("Invalid JSON body: {e}")),
                    )
                    .await;
                }
            }
        };
        let (session_id, should_start_hw, active_sessions, expires_in) = ctx
            .api_v1
            .discovery
            .create_session(parsed.client, parsed.ttl_seconds);

        if should_start_hw {
            if !ctx.airdrop.is_exclusive_active().await {
                let mut guard = ctx.rqs.lock().unwrap();
                let _ = guard.discovery(ctx.disc_tx.clone());
            }
        }
        // Always ensure AirDrop discovery runs & re-emits cached peers if AirDrop radio is active
        let _ = ctx.airdrop.start_discovery().await;

        let body = serde_json::to_vec(&serde_json::json!({
            "session_id": session_id,
            "active_sessions": active_sessions,
            "expires_in_seconds": expires_in,
        }))?;
        return write_json_status(stream, 201, &body).await;
    }

    // ---- GET /api/v1/discovery/sessions ----
    if method == "GET" && path == "/api/v1/discovery/sessions" {
        let sessions = ctx.api_v1.discovery.list();
        let body = serde_json::to_vec(&serde_json::json!({
            "active_sessions": sessions.len(),
            "sessions": sessions,
        }))?;
        return write_json_status(stream, 200, &body).await;
    }

    // ---- POST /api/v1/discovery/sessions/{id}/heartbeat ----
    if method == "POST"
        && path.starts_with("/api/v1/discovery/sessions/")
        && path.ends_with("/heartbeat")
    {
        let sid = path
            .trim_start_matches("/api/v1/discovery/sessions/")
            .trim_end_matches("/heartbeat");
        let parsed: HeartbeatReq = if req.body.trim().is_empty() {
            HeartbeatReq::default()
        } else {
            serde_json::from_str(&req.body).unwrap_or_default()
        };
        return match ctx.api_v1.discovery.heartbeat(sid, parsed.ttl_seconds) {
            Some(ttl) => {
                let body = serde_json::to_vec(&serde_json::json!({
                    "ok": true,
                    "session_id": sid,
                    "expires_in_seconds": ttl,
                }))?;
                write_json_status(stream, 200, &body).await
            }
            None => {
                write_json_status(
                    stream,
                    404,
                    &api_error_json("SESSION_NOT_FOUND", format!("Discovery session '{sid}' not found")),
                )
                .await
            }
        };
    }

    // ---- DELETE /api/v1/discovery/sessions/{id} ----
    if method == "DELETE" && path.starts_with("/api/v1/discovery/sessions/") {
        let sid = path.trim_start_matches("/api/v1/discovery/sessions/");
        if sid.is_empty() || sid.contains('/') {
            return write_json_status(
                stream,
                400,
                &api_error_json("INVALID_REQUEST", "Missing or invalid session_id"),
            )
            .await;
        }
        let (existed, should_stop_hw, active_sessions) = ctx.api_v1.discovery.remove_session(sid);
        if !existed {
            return write_json_status(
                stream,
                404,
                &api_error_json("SESSION_NOT_FOUND", format!("Discovery session '{sid}' not found")),
            )
            .await;
        }
        if should_stop_hw {
            ctx.rqs.lock().unwrap().stop_discovery();
            if !ctx.airdrop.is_exclusive_active().await {
                ctx.airdrop.stop_discovery().await;
            }
        }
        let body = serde_json::to_vec(&serde_json::json!({
            "ok": true,
            "session_id": sid,
            "active_sessions": active_sessions,
        }))?;
        return write_json_status(stream, 200, &body).await;
    }

    // ---- GET /api/v1/transfers ----
    if method == "GET" && path == "/api/v1/transfers" {
        let transfers = ctx.api_v1.transfers.list();
        let body = serde_json::to_vec(&serde_json::json!({ "transfers": transfers }))?;
        return write_json_status(stream, 200, &body).await;
    }

    // ---- POST /api/v1/transfers (idempotent send) ----
    if method == "POST" && path == "/api/v1/transfers" {
        let parsed: CreateTransferReq = match serde_json::from_str(&req.body) {
            Ok(v) => v,
            Err(e) => {
                return write_json_status(
                    stream,
                    400,
                    &api_error_json("INVALID_REQUEST", format!("Invalid JSON body: {e}")),
                )
                .await;
            }
        };

        if parsed.device_id.trim().is_empty() {
            return write_json_status(
                stream,
                400,
                &api_error_json("INVALID_REQUEST", "device_id must not be empty"),
            )
            .await;
        }

        // Check idempotency BEFORE resolving files/route so repeated requests always return
        // the original transfer_id even if the device already finished or disappeared.
        if let Some(ref rid) = parsed.request_id {
            if !rid.trim().is_empty() {
                if let Some(existing) = ctx
                    .api_v1
                    .transfers
                    .list()
                    .into_iter()
                    .find(|t| t.request_id.as_deref() == Some(rid.as_str()))
                {
                    let body = serde_json::to_vec(&serde_json::json!({
                        "transfer_id": existing.transfer_id,
                        "idempotent_replay": true,
                        "transfer": existing,
                    }))?;
                    return write_json_status(stream, 200, &body).await;
                }
            }
        }

        let expanded = expand_file_paths(&parsed.files);
        if expanded.is_empty() {
            return write_json_status(
                stream,
                422,
                &api_error_json(
                    "EMPTY_FILES",
                    "No readable files found in 'files' list",
                ),
            )
            .await;
        }

        let Some(route) = ctx.api_v1.devices.resolve_route(&parsed.device_id) else {
            let exists = ctx.api_v1.devices.get_device(&parsed.device_id).is_some();
            return write_json_status(
                stream,
                if exists { 409 } else { 404 },
                &api_error_json(
                    if exists {
                        "DEVICE_UNREACHABLE"
                    } else {
                        "DEVICE_NOT_FOUND"
                    },
                    if exists {
                        format!("Device '{}' is currently unreachable", parsed.device_id)
                    } else {
                        format!("Unknown device_id '{}'", parsed.device_id)
                    },
                ),
            )
            .await;
        };

        let file_names: Vec<String> = expanded
            .iter()
            .map(|p| {
                Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.clone())
            })
            .collect();
        let total_bytes: u64 = expanded
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
            .sum();

        let (protocol, peer_name) = match &route {
            ResolvedSendRoute::AirDrop { peer_name, .. } => ("airdrop", peer_name.clone()),
            ResolvedSendRoute::QuickShareLan { peer_name, .. }
            | ResolvedSendRoute::QuickShareBle { peer_name } => ("quickshare", peer_name.clone()),
        };

        let request_id_clean = parsed
            .request_id
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let (transfer, is_replay) = ctx.api_v1.transfers.register_outbound(
            request_id_clean,
            parsed.device_id.clone(),
            peer_name,
            protocol.into(),
            file_names,
            total_bytes,
        );

        if is_replay {
            let body = serde_json::to_vec(&serde_json::json!({
                "transfer_id": transfer.transfer_id,
                "idempotent_replay": true,
                "transfer": transfer,
            }))?;
            return write_json_status(stream, 200, &body).await;
        }

        let tid = transfer.transfer_id.clone();
        info!(
            "POST /api/v1/transfers: transfer_id={tid} device_id={} route={:?} files={}",
            parsed.device_id,
            route,
            expanded.len()
        );

        match route {
            ResolvedSendRoute::AirDrop {
                endpoint_id,
                peer_name,
            } => {
                let paths: Vec<PathBuf> = expanded.iter().map(PathBuf::from).collect();
                let ad = ctx.airdrop.clone();
                let tr_reg = ctx.api_v1.transfers.clone();
                let tid_bg = tid.clone();
                tokio::spawn(async move {
                    if let Err(e) = ad
                        .send_files_with_id(endpoint_id, peer_name, paths, Some(tid_bg.clone()))
                        .await
                    {
                        warn!("AirDrop v1 send {tid_bg} failed: {e}");
                        tr_reg.mark_failed(&tid_bg, e.to_string());
                    }
                });
            }
            ResolvedSendRoute::QuickShareLan { addr, peer_name } => {
                let _ = ctx
                    .send_tx
                    .send(SendInfo {
                        id: tid.clone(),
                        name: peer_name,
                        addr,
                        ob: OutboundPayload::Files(expanded),
                        ble: false,
                    })
                    .await;
            }
            ResolvedSendRoute::QuickShareBle { peer_name } => {
                let _ = ctx
                    .send_tx
                    .send(SendInfo {
                        id: tid.clone(),
                        name: peer_name,
                        addr: String::new(),
                        ob: OutboundPayload::Files(expanded),
                        ble: true,
                    })
                    .await;
            }
        }

        let body = serde_json::to_vec(&serde_json::json!({
            "transfer_id": tid,
            "idempotent_replay": false,
            "transfer": transfer,
        }))?;
        return write_json_status(stream, 201, &body).await;
    }

    // ---- Transfer item routes: /api/v1/transfers/{id}[/cancel|/accept|/decline] ----
    if let Some(rest) = path.strip_prefix("/api/v1/transfers/") {
        if method == "GET" && !rest.contains('/') {
            return match ctx.api_v1.transfers.get(rest) {
                Some(t) => {
                    let body = serde_json::to_vec(&serde_json::json!({ "transfer": t }))?;
                    write_json_status(stream, 200, &body).await
                }
                None => {
                    write_json_status(
                        stream,
                        404,
                        &api_error_json("TRANSFER_NOT_FOUND", format!("Transfer '{rest}' not found")),
                    )
                    .await
                }
            };
        }

        if (method == "DELETE" && !rest.contains('/'))
            || (method == "POST" && rest.ends_with("/cancel"))
        {
            let tid = rest.trim_end_matches("/cancel");
            if ctx.api_v1.transfers.get(tid).is_none() {
                return write_json_status(
                    stream,
                    404,
                    &api_error_json("TRANSFER_NOT_FOUND", format!("Transfer '{tid}' not found")),
                )
                .await;
            }
            let _ = ctx.msg_tx.send(ChannelMessage {
                id: tid.to_string(),
                msg: Message::Lib {
                    action: TransferAction::TransferCancel,
                },
            });
            let body = serde_json::to_vec(&serde_json::json!({
                "ok": true,
                "transfer_id": tid,
            }))?;
            return write_json_status(stream, 200, &body).await;
        }

        if method == "POST" && rest.ends_with("/accept") {
            let tid = rest.trim_end_matches("/accept");
            if ctx.api_v1.transfers.get(tid).is_none() {
                return write_json_status(
                    stream,
                    404,
                    &api_error_json("TRANSFER_NOT_FOUND", format!("Transfer '{tid}' not found")),
                )
                .await;
            }
            let _ = ctx.msg_tx.send(ChannelMessage {
                id: tid.to_string(),
                msg: Message::Lib {
                    action: TransferAction::ConsentAccept,
                },
            });
            let body = serde_json::to_vec(&serde_json::json!({
                "ok": true,
                "transfer_id": tid,
            }))?;
            return write_json_status(stream, 200, &body).await;
        }

        if method == "POST" && rest.ends_with("/decline") {
            let tid = rest.trim_end_matches("/decline");
            if ctx.api_v1.transfers.get(tid).is_none() {
                return write_json_status(
                    stream,
                    404,
                    &api_error_json("TRANSFER_NOT_FOUND", format!("Transfer '{tid}' not found")),
                )
                .await;
            }
            let _ = ctx.msg_tx.send(ChannelMessage {
                id: tid.to_string(),
                msg: Message::Lib {
                    action: TransferAction::ConsentDecline,
                },
            });
            let body = serde_json::to_vec(&serde_json::json!({
                "ok": true,
                "transfer_id": tid,
            }))?;
            return write_json_status(stream, 200, &body).await;
        }
    }

    // ---- POST /api/v1/visibility ----
    if method == "POST" && path == "/api/v1/visibility" {
        let parsed: VisibilityReq = match serde_json::from_str(&req.body) {
            Ok(v) => v,
            Err(e) => {
                return write_json_status(
                    stream,
                    400,
                    &api_error_json("INVALID_REQUEST", format!("Invalid JSON body: {e}")),
                )
                .await;
            }
        };
        let nv = match parsed.visibility.as_str() {
            "Visible" | "visible" => Visibility::Visible,
            "Temporarily" | "temporarily" => Visibility::Temporarily,
            "Invisible" | "invisible" => Visibility::Invisible,
            other => {
                return write_json_status(
                    stream,
                    400,
                    &api_error_json(
                        "INVALID_REQUEST",
                        format!("Invalid visibility '{other}'. Expected Visible, Temporarily, or Invisible."),
                    ),
                )
                .await;
            }
        };
        ctx.rqs.lock().unwrap().change_visibility(nv);
        let body = serde_json::to_vec(&serde_json::json!({ "ok": true, "visibility": nv }))?;
        return write_json_status(stream, 200, &body).await;
    }

    // ---- POST /api/v1/airdrop/enable | /disable ----
    if method == "POST" && path == "/api/v1/airdrop/enable" {
        ctx.rqs.lock().unwrap().stop_discovery();
        return match ctx.airdrop.enable().await {
            Ok(st) => {
                let body = serde_json::to_vec(&serde_json::json!({ "ok": true, "airdrop": st }))?;
                write_json_status(stream, 200, &body).await
            }
            Err(e) => {
                write_json_status(
                    stream,
                    409,
                    &api_error_json("AIRDROP_UNAVAILABLE", e.to_string()),
                )
                .await
            }
        };
    }

    if method == "POST" && path == "/api/v1/airdrop/disable" {
        let st = ctx
            .airdrop
            .disable()
            .await
            .unwrap_or_default();
        if ctx.api_v1.discovery.active_count() > 0 {
            let mut guard = ctx.rqs.lock().unwrap();
            let _ = guard.discovery(ctx.disc_tx.clone());
        }
        let body = serde_json::to_vec(&serde_json::json!({ "ok": true, "airdrop": st }))?;
        return write_json_status(stream, 200, &body).await;
    }

    // ---- GET /api/v1/events (SSE with immediate snapshot + live normalized events) ----
    if method == "GET" && path == "/api/v1/events" {
        let hdr = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nAccess-Control-Allow-Origin: *\r\n\r\n";
        stream.write_all(hdr.as_bytes()).await?;

        // Subscribe BEFORE taking the snapshot so no event between snapshot and stream is missed.
        let mut dev_rx = ctx.api_v1.devices.subscribe();
        let mut tr_rx = ctx.api_v1.transfers.subscribe();

        let snap = serde_json::json!({
            "devices": ctx.api_v1.devices.list_reachable(),
            "transfers": ctx.api_v1.transfers.list(),
            "active_discovery_sessions": ctx.api_v1.discovery.active_count(),
        });
        let snap_pkt = format!("event: snapshot\ndata: {}\n\n", serde_json::to_string(&snap)?);
        stream.write_all(snap_pkt.as_bytes()).await?;
        stream.flush().await?;

        let mut keepalive = tokio::time::interval(Duration::from_secs(15));
        keepalive.tick().await;

        loop {
            tokio::select! {
                _ = keepalive.tick() => {
                    if stream.write_all(b": keepalive\n\n").await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;
                }
                res = dev_rx.recv() => match res {
                    Ok(ev) => {
                        if let Ok(j) = serde_json::to_string(&ev) {
                            let pkt = format!("event: device\ndata: {j}\n\n");
                            if stream.write_all(pkt.as_bytes()).await.is_err() { break; }
                            let _ = stream.flush().await;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                res = tr_rx.recv() => match res {
                    Ok(tr) => {
                        if let Ok(j) = serde_json::to_string(&tr) {
                            let pkt = format!("event: transfer\ndata: {j}\n\n");
                            if stream.write_all(pkt.as_bytes()).await.is_err() { break; }
                            let _ = stream.flush().await;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
        return Ok(());
    }

    write_json_status(
        stream,
        404,
        &api_error_json("NOT_FOUND", format!("Unknown endpoint: {method} {path}")),
    )
    .await
}
