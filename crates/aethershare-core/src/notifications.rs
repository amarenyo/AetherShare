//! Interactive Linux desktop notifications for AetherShare transfers.
//!
//! - Incoming consent notification with native `[Annehmen]` / `[Ablehnen]` action buttons
//!   via FreeDesktop `notify-send` (libnotify 0.8+).
//! - Live native progress bar notification:
//!   * On KDE Plasma 5/6: uses a persistent D-Bus session connection to `org.kde.kuiserver`
//!     (`org.kde.JobViewServer` / `org.kde.JobViewV2`) so Plasma keeps the native progress bar
//!     alive for the full duration of the transfer (including Stop/Cancel button support!).
//!   * On GNOME / Dunst / Mako / other desktops: falls back cleanly to `notify-send -r <id> -h int:value:<pct>`.
//! - Completion notification with a native `[Open]` button (`xdg-open`).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use log::{debug, info};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, broadcast};

use crate::TransferState;
use crate::channel::{ChannelMessage, Message, TransferAction, TransferKind};
use crate::hdl::info::TransferPayload;
use crate::i18n;

struct ActiveNotif {
    peer: String,
    files: String,
    kind: TransferKind,
    total_bytes: u64,
    saved_path: Option<String>,
    destination: Option<String>,
    started_at: Instant,
    last_update: Instant,
    last_pct: u32,
    /// KDE Plasma native JobViewV2 D-Bus object path.
    kde_job_path: Option<String>,
    /// Fallback FreeDesktop notification ID for non-KDE desktops.
    fallback_notif_id: Option<u32>,
    /// Running consent `notify-send` task (cancelled if user accepts/declines in app window).
    consent_task: Option<tokio::task::JoinHandle<()>>,
    /// FreeDesktop notification ID of the consent popup so we can dismiss it if handled in-app.
    consent_notif_id: Arc<Mutex<Option<u32>>>,
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut val = bytes as f64;
    let mut idx = 0usize;
    while val >= 1024.0 && idx < UNITS.len() - 1 {
        val /= 1024.0;
        idx += 1;
    }
    if idx == 0 || val >= 10.0 {
        format!("{val:.0} {}", UNITS[idx])
    } else {
        format!("{val:.1} {}", UNITS[idx])
    }
}

fn describe_payload(payload: Option<&TransferPayload>) -> String {
    let s = i18n::strings();
    match payload {
        Some(TransferPayload::Files(f)) if !f.is_empty() => {
            if f.len() == 1 {
                f[0].clone()
            } else {
                format!("{} (+{} {})", f[0], f.len() - 1, s.more_suffix)
            }
        }
        Some(TransferPayload::Text(t)) => format!("{}: {t}", s.text_prefix),
        Some(TransferPayload::Url(u)) => format!("{}: {u}", s.link_prefix),
        Some(TransferPayload::Wifi { ssid, .. }) => format!("{}: {ssid}", s.wifi_prefix),
        _ => s.files_fallback.to_string(),
    }
}

#[cfg(target_os = "linux")]
mod dbus_kde {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use dbus::Path;
    use dbus::blocking::Connection;
    use dbus::message::MatchRule;
    use log::{debug, info};
    use tokio::sync::{broadcast, oneshot};

    use crate::channel::{ChannelMessage, Message, TransferAction};

    const TIMEOUT: Duration = Duration::from_millis(800);

    enum KdeCmd {
        CloseFdo(u32),
        Create {
            transfer_id: String,
            info_msg: String,
            peer: String,
            files: String,
            total_bytes: u64,
            reply: oneshot::Sender<Option<String>>,
        },
        Update {
            path: String,
            ack_bytes: u64,
            total_bytes: u64,
            pct: u32,
            speed_bps: u64,
        },
        Terminate {
            path: String,
            error_msg: String,
        },
    }

    #[derive(Clone)]
    pub struct KdeJobClient {
        tx: mpsc::Sender<KdeCmd>,
    }

    impl KdeJobClient {
        /// Starts ONE persistent D-Bus session connection thread.
        /// KDE Plasma destroys any JobView immediately when its owning D-Bus connection drops,
        /// so keeping a single long-lived connection is mandatory for progress bars to stay visible.
        pub fn start(msg_tx: broadcast::Sender<ChannelMessage>) -> Self {
            let (tx, rx) = mpsc::channel::<KdeCmd>();
            std::thread::Builder::new()
                .name("aethershare-kde-notif".to_string())
                .spawn(move || {
                    let Ok(conn) = Connection::new_session() else {
                        debug!("KDE JobView worker: could not connect to D-Bus session bus");
                        return;
                    };

                    // Map JobView object path -> transfer_id so KDE Stop button cancels the right transfer
                    let path_to_transfer: Arc<Mutex<HashMap<String, String>>> =
                        Arc::new(Mutex::new(HashMap::new()));

                    let rule = MatchRule::new_signal("org.kde.JobViewV2", "cancelRequested");
                    let map_cb = path_to_transfer.clone();
                    let tx_cb = msg_tx.clone();
                    let _ = conn.add_match(rule, move |_: (), _conn, msg| {
                        if let Some(p) = msg.path() {
                            let ps = p.to_string();
                            if let Some(tid) = map_cb.lock().unwrap().get(&ps).cloned() {
                                info!("User cancelled transfer {tid} via KDE notification progress bar");
                                let _ = tx_cb.send(ChannelMessage {
                                    id: tid,
                                    msg: Message::Lib {
                                        action: TransferAction::TransferCancel,
                                    },
                                });
                            }
                        }
                        true
                    });

                    loop {
                        match rx.recv_timeout(Duration::from_millis(40)) {
                            Ok(KdeCmd::CloseFdo(id)) => {
                                let proxy = conn.with_proxy(
                                    "org.freedesktop.Notifications",
                                    "/org/freedesktop/Notifications",
                                    TIMEOUT,
                                );
                                let _: Result<(), _> = proxy.method_call(
                                    "org.freedesktop.Notifications",
                                    "CloseNotification",
                                    (id,),
                                );
                            }
                            Ok(KdeCmd::Create {
                                transfer_id,
                                info_msg,
                                peer,
                                files,
                                total_bytes,
                                reply,
                            }) => {
                                let server = conn.with_proxy(
                                    "org.kde.kuiserver",
                                    "/JobViewServer",
                                    TIMEOUT,
                                );
                                // capabilities = 1 (Killable -> shows Stop/Cancel button in KDE progress card)
                                let created: Option<String> = server
                                    .method_call::<(Path<'static>,), _, _, _>(
                                        "org.kde.JobViewServer",
                                        "requestView",
                                        ("AetherShare", "aethershare", 1i32),
                                    )
                                    .ok()
                                    .map(|(p,)| p.to_string());

                                if let Some(ref path_str) = created {
                                    path_to_transfer
                                        .lock()
                                        .unwrap()
                                        .insert(path_str.clone(), transfer_id);
                                    let view = conn.with_proxy(
                                        "org.kde.kuiserver",
                                        path_str.as_str(),
                                        TIMEOUT,
                                    );
                                    let _: Result<(), _> = view.method_call(
                                        "org.kde.JobViewV2",
                                        "setInfoMessage",
                                        (info_msg.as_str(),),
                                    );
                                    let s = crate::i18n::strings();
                                    let _: Result<(), _> = view.method_call(
                                        "org.kde.JobViewV2",
                                        "setDescriptionField",
                                        (0u32, s.field_device, peer.as_str()),
                                    );
                                    let _: Result<(), _> = view.method_call(
                                        "org.kde.JobViewV2",
                                        "setDescriptionField",
                                        (1u32, s.field_file, files.as_str()),
                                    );
                                    if total_bytes > 0 {
                                        let _: Result<(), _> = view.method_call(
                                            "org.kde.JobViewV2",
                                            "setTotalAmount",
                                            (total_bytes, "bytes"),
                                        );
                                        let _: Result<(), _> = view.method_call(
                                            "org.kde.JobViewV2",
                                            "setProcessedAmount",
                                            (0u64, "bytes"),
                                        );
                                    }
                                    let _: Result<(), _> = view.method_call(
                                        "org.kde.JobViewV2",
                                        "setPercent",
                                        (0u32,),
                                    );
                                }
                                let _ = reply.send(created);
                            }
                            Ok(KdeCmd::Update {
                                path,
                                ack_bytes,
                                total_bytes,
                                pct,
                                speed_bps,
                            }) => {
                                let view = conn.with_proxy(
                                    "org.kde.kuiserver",
                                    path.as_str(),
                                    TIMEOUT,
                                );
                                if total_bytes > 0 {
                                    let _: Result<(), _> = view.method_call(
                                        "org.kde.JobViewV2",
                                        "setTotalAmount",
                                        (total_bytes, "bytes"),
                                    );
                                    let _: Result<(), _> = view.method_call(
                                        "org.kde.JobViewV2",
                                        "setProcessedAmount",
                                        (ack_bytes, "bytes"),
                                    );
                                }
                                let _: Result<(), _> =
                                    view.method_call("org.kde.JobViewV2", "setPercent", (pct,));
                                if speed_bps > 0 {
                                    let _: Result<(), _> = view.method_call(
                                        "org.kde.JobViewV2",
                                        "setSpeed",
                                        (speed_bps,),
                                    );
                                }
                            }
                            Ok(KdeCmd::Terminate { path, error_msg }) => {
                                path_to_transfer.lock().unwrap().remove(&path);
                                let view = conn.with_proxy(
                                    "org.kde.kuiserver",
                                    path.as_str(),
                                    TIMEOUT,
                                );
                                let _: Result<(), _> = view.method_call(
                                    "org.kde.JobViewV2",
                                    "terminate",
                                    (error_msg.as_str(),),
                                );
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                        // Pump incoming D-Bus signals (e.g., cancelRequested from Plasma)
                        let _ = conn.process(Duration::from_millis(10));
                    }
                })
                .ok();
            Self { tx }
        }

        pub fn close_fdo_notification(&self, id: u32) {
            let _ = self.tx.send(KdeCmd::CloseFdo(id));
        }

        pub async fn create_job_view(
            &self,
            transfer_id: String,
            info_msg: String,
            peer: String,
            files: String,
            total_bytes: u64,
        ) -> Option<String> {
            let (reply, rx) = oneshot::channel();
            self.tx
                .send(KdeCmd::Create {
                    transfer_id,
                    info_msg,
                    peer,
                    files,
                    total_bytes,
                    reply,
                })
                .ok()?;
            rx.await.ok().flatten()
        }

        pub fn update_job_view(
            &self,
            path: String,
            ack_bytes: u64,
            total_bytes: u64,
            pct: u32,
            speed_bps: u64,
        ) {
            let _ = self.tx.send(KdeCmd::Update {
                path,
                ack_bytes,
                total_bytes,
                pct,
                speed_bps,
            });
        }

        pub fn terminate_job_view(&self, path: String, error_msg: String) {
            let _ = self.tx.send(KdeCmd::Terminate { path, error_msg });
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod dbus_kde {
    use tokio::sync::broadcast;

    use crate::channel::ChannelMessage;

    #[derive(Clone)]
    pub struct KdeJobClient;

    impl KdeJobClient {
        pub fn start(_msg_tx: broadcast::Sender<ChannelMessage>) -> Self {
            Self
        }
        pub fn close_fdo_notification(&self, _id: u32) {}
        pub async fn create_job_view(
            &self,
            _transfer_id: String,
            _info_msg: String,
            _peer: String,
            _files: String,
            _total_bytes: u64,
        ) -> Option<String> {
            None
        }
        pub fn update_job_view(
            &self,
            _path: String,
            _ack_bytes: u64,
            _total_bytes: u64,
            _pct: u32,
            _speed_bps: u64,
        ) {
        }
        pub fn terminate_job_view(&self, _path: String, _error_msg: String) {}
    }
}

/// Spawns the interactive Linux desktop notification manager for all Quick Share & AirDrop transfers.
pub fn spawn_transfer_notifications(
    msg_tx: broadcast::Sender<ChannelMessage>,
    auto_accept: Arc<AtomicBool>,
) {
    let kde = dbus_kde::KdeJobClient::start(msg_tx.clone());
    let mut rx = msg_tx.subscribe();
    let active: Arc<Mutex<HashMap<String, ActiveNotif>>> = Arc::new(Mutex::new(HashMap::new()));

    tokio::spawn(async move {
        while let Ok(cm) = rx.recv().await {
            let Message::Client(client) = &cm.msg else {
                continue;
            };
            let Some(state) = &client.state else {
                continue;
            };

            let s = i18n::strings();
            let meta = client.metadata.as_ref();
            let peer = meta
                .and_then(|m| m.source.as_ref())
                .map(|src| src.name.clone())
                .unwrap_or_else(|| s.device_fallback.to_string());
            let pin = meta
                .and_then(|m| m.pin_code.as_deref())
                .unwrap_or("-")
                .to_string();
            let files_desc = describe_payload(meta.and_then(|m| m.payload.as_ref()));
            let total_bytes = meta.map(|m| m.total_bytes).unwrap_or(0);
            let ack_bytes = meta.map(|m| m.ack_bytes).unwrap_or(0);
            let saved_path = meta
                .and_then(|m| m.saved_paths.as_ref())
                .and_then(|v| v.first())
                .cloned();
            let destination = meta.and_then(|m| m.destination.clone());

            match state {
                TransferState::WaitingForUserConsent => {
                    info!(
                        ">>> EINGEHENDER TRANSFER von '{peer}' (PIN: {pin}): {files_desc} (ID: {})",
                        cm.id
                    );

                    if auto_accept.load(Ordering::Relaxed) && !cm.id.starts_with("airdrop-") {
                        info!("Auto-accepting Quick Share transfer {}", cm.id);
                        let _ = msg_tx.send(ChannelMessage {
                            id: cm.id.clone(),
                            msg: Message::Lib {
                                action: TransferAction::ConsentAccept,
                            },
                        });
                        continue;
                    }

                    let id_clone = cm.id.clone();
                    let tx_clone = msg_tx.clone();
                    let consent_notif_id = Arc::new(Mutex::new(None::<u32>));
                    let cid_ref = consent_notif_id.clone();
                    let peer_for_notif = peer.clone();
                    let files_for_notif = files_desc.clone();
                    let size_suffix = if total_bytes > 0 {
                        format!(" ({})", format_bytes(total_bytes))
                    } else {
                        String::new()
                    };

                    let handle = tokio::spawn(async move {
                        let s = i18n::strings();
                        let Ok(mut child) = tokio::process::Command::new("notify-send")
                            .args([
                                "-a",
                                "AetherShare",
                                "-i",
                                "aethershare",
                                "-u",
                                "normal",
                                "-p",
                                "-t",
                                "30000",
                                "-A",
                                &format!("accept={}", s.accept),
                                "-A",
                                &format!("decline={}", s.decline),
                                &format!("{} {peer_for_notif}", s.incoming_title_prefix),
                                &format!("{files_for_notif}{size_suffix}\nPIN: {pin}"),
                            ])
                            .stdout(Stdio::piped())
                            .stderr(Stdio::null())
                            .kill_on_drop(true)
                            .spawn()
                        else {
                            return;
                        };

                        let Some(stdout) = child.stdout.take() else {
                            return;
                        };
                        let mut lines = BufReader::new(stdout).lines();

                        if let Ok(Some(line1)) = lines.next_line().await {
                            if let Ok(nid) = line1.trim().parse::<u32>() {
                                *cid_ref.lock().await = Some(nid);
                            } else if line1.trim() == "accept" {
                                let _ = tx_clone.send(ChannelMessage {
                                    id: id_clone,
                                    msg: Message::Lib {
                                        action: TransferAction::ConsentAccept,
                                    },
                                });
                                return;
                            } else if line1.trim() == "decline" {
                                let _ = tx_clone.send(ChannelMessage {
                                    id: id_clone,
                                    msg: Message::Lib {
                                        action: TransferAction::ConsentDecline,
                                    },
                                });
                                return;
                            }
                        }

                        if let Ok(Some(action_line)) = lines.next_line().await {
                            match action_line.trim() {
                                "accept" => {
                                    info!("User accepted transfer {id_clone} via notification");
                                    let _ = tx_clone.send(ChannelMessage {
                                        id: id_clone,
                                        msg: Message::Lib {
                                            action: TransferAction::ConsentAccept,
                                        },
                                    });
                                }
                                "decline" => {
                                    info!("User declined transfer {id_clone} via notification");
                                    let _ = tx_clone.send(ChannelMessage {
                                        id: id_clone,
                                        msg: Message::Lib {
                                            action: TransferAction::ConsentDecline,
                                        },
                                    });
                                }
                                _ => {}
                            }
                        }
                        let _ = child.wait().await;
                    });

                    let mut map = active.lock().await;
                    map.insert(
                        cm.id.clone(),
                        ActiveNotif {
                            peer,
                            files: files_desc,
                            kind: client.kind.clone(),
                            total_bytes,
                            saved_path,
                            destination,
                            started_at: Instant::now(),
                            last_update: Instant::now() - Duration::from_secs(10),
                            last_pct: u32::MAX,
                            kde_job_path: None,
                            fallback_notif_id: None,
                            consent_task: Some(handle),
                            consent_notif_id,
                        },
                    );
                }

                TransferState::ReceivingFiles | TransferState::SendingFiles => {
                    let mut map = active.lock().await;
                    let entry = map.entry(cm.id.clone()).or_insert_with(|| ActiveNotif {
                        peer: peer.clone(),
                        files: files_desc.clone(),
                        kind: client.kind.clone(),
                        total_bytes,
                        saved_path: saved_path.clone(),
                        destination: destination.clone(),
                        started_at: Instant::now(),
                        last_update: Instant::now() - Duration::from_secs(10),
                        last_pct: u32::MAX,
                        kde_job_path: None,
                        fallback_notif_id: None,
                        consent_task: None,
                        consent_notif_id: Arc::new(Mutex::new(None)),
                    });

                    if let Some(task) = entry.consent_task.take() {
                        task.abort();
                    }
                    if let Some(cid) = entry.consent_notif_id.lock().await.take() {
                        kde.close_fdo_notification(cid);
                    }

                    if total_bytes > 0 {
                        entry.total_bytes = total_bytes;
                    }
                    if saved_path.is_some() {
                        entry.saved_path = saved_path;
                    }
                    if destination.is_some() {
                        entry.destination = destination;
                    }
                    if !peer.is_empty() && peer != s.device_fallback {
                        entry.peer = peer;
                    }
                    if !files_desc.is_empty() && files_desc != s.files_fallback {
                        entry.files = files_desc;
                    }

                    let pct = if entry.total_bytes > 0 {
                        ((ack_bytes.saturating_mul(100)) / entry.total_bytes).min(100) as u32
                    } else {
                        0
                    };

                    if entry.kde_job_path.is_none() && entry.fallback_notif_id.is_none() {
                        let info_msg = match entry.kind {
                            TransferKind::Inbound => format!("{} {}", s.receiving_from, entry.peer),
                            TransferKind::Outbound => format!("{} {}", s.sending_to, entry.peer),
                        };
                        let job_path = kde
                            .create_job_view(
                                cm.id.clone(),
                                info_msg,
                                entry.peer.clone(),
                                entry.files.clone(),
                                entry.total_bytes,
                            )
                            .await;

                        if let Some(jp) = job_path {
                            debug!("Opened persistent KDE JobViewV2 progress notification: {jp}");
                            entry.kde_job_path = Some(jp);
                            entry.started_at = Instant::now();
                        } else if let Some(nid) = send_fallback_progress(
                            None,
                            &entry.kind,
                            &entry.peer,
                            &entry.files,
                            pct,
                            ack_bytes,
                            entry.total_bytes,
                        )
                        .await
                        {
                            entry.fallback_notif_id = Some(nid);
                        }
                        entry.last_pct = pct;
                        entry.last_update = Instant::now();
                        continue;
                    }

                    let pct_diff = pct.abs_diff(entry.last_pct);
                    if pct_diff < 2
                        && entry.last_update.elapsed() < Duration::from_millis(160)
                        && pct < 100
                    {
                        continue;
                    }
                    entry.last_pct = pct;
                    entry.last_update = Instant::now();

                    let elapsed_secs = entry.started_at.elapsed().as_secs_f64().max(0.05);
                    let speed_bps = (ack_bytes as f64 / elapsed_secs) as u64;

                    if let Some(jp) = entry.kde_job_path.clone() {
                        kde.update_job_view(jp, ack_bytes, entry.total_bytes, pct, speed_bps);
                    } else if let Some(nid) = entry.fallback_notif_id {
                        let _ = send_fallback_progress(
                            Some(nid),
                            &entry.kind,
                            &entry.peer,
                            &entry.files,
                            pct,
                            ack_bytes,
                            entry.total_bytes,
                        )
                        .await;
                    }
                }

                TransferState::Finished => {
                    info!(">>> TRANSFER ABGESCHLOSSEN (ID: {})", cm.id);
                    let entry = active.lock().await.remove(&cm.id);

                    let (title, body, open_target, fallback_id) = if let Some(mut e) = entry {
                        if let Some(task) = e.consent_task.take() {
                            task.abort();
                        }
                        if let Some(jp) = e.kde_job_path.take() {
                            kde.update_job_view(jp.clone(), e.total_bytes, e.total_bytes, 100, 0);
                            kde.terminate_job_view(jp, String::new());
                        }
                        let title = match e.kind {
                            TransferKind::Inbound => format!("{} {}", s.received_from, e.peer),
                            TransferKind::Outbound => format!("{} {}", s.sent_to, e.peer),
                        };
                        let target = e
                            .saved_path
                            .or(saved_path)
                            .or(e.destination)
                            .or(destination);
                        (title, e.files, target, e.fallback_notif_id)
                    } else {
                        (
                            format!("{} ({peer})", s.transfer_completed),
                            files_desc,
                            saved_path.or(destination),
                            None,
                        )
                    };

                    tokio::spawn(async move {
                        let s = i18n::strings();
                        let mut cmd = tokio::process::Command::new("notify-send");
                        cmd.args(["-a", "AetherShare", "-i", "aethershare", "-t", "8000"]);
                        if let Some(nid) = fallback_id {
                            cmd.args(["-r", &nid.to_string()]);
                        }
                        if open_target.is_some() {
                            cmd.args(["-A", &format!("open={}", s.open)]);
                        }
                        cmd.args([&title, &body]);
                        cmd.stdout(Stdio::piped()).stderr(Stdio::null());

                        if let Ok(out) = cmd.output().await {
                            let chosen = String::from_utf8_lossy(&out.stdout);
                            if chosen.trim() == "open" {
                                if let Some(path) = open_target {
                                    let _ =
                                        tokio::process::Command::new("xdg-open").arg(&path).spawn();
                                }
                            }
                        }
                    });
                }

                TransferState::Rejected | TransferState::Cancelled | TransferState::Disconnected => {
                    if let Some(mut e) = active.lock().await.remove(&cm.id) {
                        if let Some(task) = e.consent_task.take() {
                            task.abort();
                        }
                        if let Some(cid) = e.consent_notif_id.lock().await.take() {
                            kde.close_fdo_notification(cid);
                        }
                        if let Some(jp) = e.kde_job_path.take() {
                            let err_msg = match state {
                                TransferState::Rejected => s.transfer_rejected,
                                TransferState::Cancelled => s.transfer_cancelled,
                                _ => "",
                            };
                            kde.terminate_job_view(jp, err_msg.to_string());
                        }
                    }
                }

                _ => {}
            }
        }
    });
}

async fn send_fallback_progress(
    replace_id: Option<u32>,
    kind: &TransferKind,
    peer: &str,
    files: &str,
    pct: u32,
    ack_bytes: u64,
    total_bytes: u64,
) -> Option<u32> {
    let s = i18n::strings();
    let title = match kind {
        TransferKind::Inbound => format!("{} {peer} ({pct} %)", s.receiving_from),
        TransferKind::Outbound => format!("{} {peer} ({pct} %)", s.sending_to),
    };
    let body = if total_bytes > 0 {
        format!("{files} • {} / {}", format_bytes(ack_bytes), format_bytes(total_bytes))
    } else {
        files.to_string()
    };

    let mut cmd = tokio::process::Command::new("notify-send");
    cmd.args([
        "-a",
        "AetherShare",
        "-i",
        "aethershare",
        "-p",
        "-t",
        "5000",
        "-h",
        &format!("int:value:{pct}"),
        "-h",
        "boolean:transient:true",
    ]);
    if let Some(rid) = replace_id {
        cmd.args(["-r", &rid.to_string()]);
    }
    cmd.args([&title, &body]);
    let out = cmd.output().await.ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok()
}
