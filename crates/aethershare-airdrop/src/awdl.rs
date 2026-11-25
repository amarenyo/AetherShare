//! Real Apple AirDrop over AWDL (`awdl0` + IPv6 link-local + Apple BLE wake-up),
//! powered by the MIT-licensed `owly-airdrop` & `owlyd` crates (<https://github.com/t4t5/owly>).
//!
//! Architecture:
//! - `aethershare-awdld` (system service implementing `io.github.t4t5.Owly.Radio1`) owns
//!   the raw Wi-Fi radio and creates `awdl0` on demand, automatically restoring Wi-Fi on
//!   release or crash.
//! - [`AirDropManager`] runs completely unprivileged inside `aethershare-desktop` /
//!   `aethershare`: it probes card capabilities (`Shared` vs `Exclusive`), acquires/releases
//!   the AWDL radio lease, runs `owly_airdrop::driver::receiver::Receiver` + burst mDNS on
//!   `awdl0:8771`, wakes nearby iPhones/Macs with an Apple Continuity BLE `0x004C` advert,
//!   discovers Apple receivers via both mDNS and AWDL peer `/Discover` (port `8770`), and
//!   bridges all transfers into AetherShare's `ChannelMessage` stream.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{Ipv6Addr, SocketAddrV6};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use aethershare_core::channel::{
    ChannelMessage, Message, MessageClient, TransferAction, TransferKind,
};
use aethershare_core::hdl::info::{TransferMetadata, TransferPayload, TransferPayloadKind};
use aethershare_core::utils::{DeviceType, RemoteDeviceInfo, get_download_dir};
use aethershare_core::{EndpointInfo, TransferState};
use anyhow::{Result, anyhow};
use log::{debug, info, warn};
use owly_airdrop::driver::mdns::Mdns;
use owly_airdrop::driver::receiver::{Options as ReceiverOptions, Receiver, ReceiverEvent};
use owly_airdrop::driver::sender::{Event as SenderEvent, Payload, Sender};
use owly_airdrop::driver::tls::Identity;
use owly_airdrop::proto::ble::{APPLE_COMPANY_ID, Advert, INTERVAL_MS};
use owly_airdrop::proto::mdns::{
    self, BrowseEvent, RECEIVER_FLAGS, SUPPORTS_DVZIP, Service as MdnsService,
};
use owly_airdrop::proto::messages::Offer;
use owly_airdrop::proto::receiver::Config as ReceiverConfig;
use owly_airdrop::proto::sender::{self, Outcome};
use owly_awdl::MacAddr;
use owly_radio::radio::Announce;
use owlyd::api::{self, LinkInfo, RadioProxy};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, Value};

/// Standard Apple AirDrop receiver port on iPhones, iPads, and Macs.
pub const APPLE_AIRDROP_PORT: u16 = 8770;
/// Local AirDrop receiver port on `awdl0`.
pub const LOCAL_AIRDROP_PORT: u16 = 8771;

/// High-level status of the embedded AWDL / AirDrop radio subsystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AirDropRadioStatus {
    /// True if `aethershare-awdld` (`io.github.t4t5.Owly`) is reachable on System D-Bus
    /// and the local Wi-Fi hardware is supported.
    pub available: bool,
    /// True while an AWDL radio lease is currently active (`awdl0` up).
    pub active: bool,
    /// `"shared"` (Wi-Fi stays connected), `"exclusive"` (pauses Wi-Fi while active),
    /// `"unsupported"`, or `"unavailable"`.
    pub mode: String,
    pub driver: Option<String>,
    pub device: Option<String>,
    pub channel: Option<u8>,
    pub reason: Option<String>,
}

impl Default for AirDropRadioStatus {
    fn default() -> Self {
        Self {
            available: false,
            active: false,
            mode: "unavailable".into(),
            driver: None,
            device: None,
            channel: None,
            reason: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ProbeReport {
    driver: Option<String>,
    device: Option<String>,
    mode: Option<String>,
    channel: Option<u8>,
    #[serde(default)]
    hardware: bool,
    #[serde(default)]
    missing: Vec<ProbeMissing>,
}

#[derive(Debug, Deserialize)]
struct ProbeMissing {
    say: String,
}

struct ActiveSession {
    lease_id: u64,
    radio: RadioProxy<'static>,
    link: LinkInfo,
    session_ctk: CancellationToken,
    disc_ctk: Option<CancellationToken>,
}

/// Manages the real AWDL AirDrop radio lease, receiver, discovery, Apple BLE wake-up,
/// and outbound file transfers.
#[derive(Clone)]
pub struct AirDropManager {
    inner: Arc<Mutex<Option<ActiveSession>>>,
    cached_status: Arc<Mutex<AirDropRadioStatus>>,
    device_name: Arc<Mutex<String>>,
    auto_accept: Arc<AtomicBool>,
    msg_tx: broadcast::Sender<ChannelMessage>,
    disc_tx: broadcast::Sender<EndpointInfo>,
    /// Stores discovered AirDrop peer flags keyed by endpoint id (`airdrop://...`).
    peer_flags: Arc<Mutex<HashMap<String, u32>>>,
}

impl AirDropManager {
    pub fn new(
        device_name: String,
        auto_accept: Arc<AtomicBool>,
        msg_tx: broadcast::Sender<ChannelMessage>,
        disc_tx: broadcast::Sender<EndpointInfo>,
    ) -> Self {
        // Ensure rustls has its ring crypto provider installed for owly-airdrop TLS.
        let _ = rustls::crypto::ring::default_provider().install_default();

        Self {
            inner: Arc::new(Mutex::new(None)),
            cached_status: Arc::new(Mutex::new(AirDropRadioStatus::default())),
            device_name: Arc::new(Mutex::new(device_name)),
            auto_accept,
            msg_tx,
            disc_tx,
            peer_flags: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn set_device_name(&self, name: String) {
        *self.device_name.lock().await = name;
    }

    /// Returns the last probed status updated with current `active` state.
    pub async fn status(&self) -> AirDropRadioStatus {
        let active = self.inner.lock().await.is_some();
        let mut st = self.cached_status.lock().await.clone();
        st.active = active;
        st
    }

    /// Probes `aethershare-awdld` (`io.github.t4t5.Owly.Radio1`) on the system D-Bus
    /// to determine whether the local Wi-Fi card supports Shared or Exclusive AWDL mode.
    pub async fn probe(&self) -> AirDropRadioStatus {
        let active = self.inner.lock().await.is_some();
        let status = match probe_owlyd().await {
            Ok(mut st) => {
                st.active = active;
                st
            }
            Err(e) => AirDropRadioStatus {
                available: false,
                active,
                mode: "unavailable".into(),
                driver: None,
                device: None,
                channel: None,
                reason: Some(e.to_string()),
            },
        };
        *self.cached_status.lock().await = status.clone();
        status
    }

    /// Acquires an AWDL radio lease from `aethershare-awdld` and starts the real
    /// `owly-airdrop` receiver + mDNS burst responder on `awdl0:8771`.
    pub async fn enable(&self) -> Result<AirDropRadioStatus> {
        {
            if self.inner.lock().await.is_some() {
                return Ok(self.status().await);
            }
        }

        let conn = api::connect(None)
            .await
            .map_err(|e| anyhow!("AWDL radio service unavailable on system D-Bus: {e}"))?;
        let radio = api::radio(&conn)
            .await
            .map_err(|e| anyhow!("Failed to bind AWDL radio proxy: {e}"))?;

        info!("AirDropManager: requesting AWDL radio lease from aethershare-awdld...");
        let (lease_id, link_dict) = radio
            .acquire(0)
            .await
            .map_err(|e| anyhow!("Failed to acquire AWDL radio: {e}"))?;
        let link = LinkInfo::from_dict(&link_dict)
            .ok_or_else(|| anyhow!("AWDL radio returned incomplete link info"))?;
        let ipv6: Ipv6Addr = link
            .address
            .parse()
            .map_err(|e| anyhow!("Invalid awdl0 IPv6 address {:?}: {e}", link.address))?;

        info!(
            "AirDropManager: AWDL lease #{lease_id} active on {} (index={}, addr={})",
            link.interface, link.index, ipv6
        );

        let session_ctk = CancellationToken::new();
        let device_name = self.device_name.lock().await.clone();

        // Spawn real owly-airdrop Receiver + mDNS burst responder on awdl0:8771
        let recv_ctk = session_ctk.clone();
        let recv_link = link.clone();
        let recv_msg_tx = self.msg_tx.clone();
        let recv_auto_accept = self.auto_accept.clone();
        tokio::spawn(async move {
            if let Err(e) = run_awdl_receiver(
                recv_link,
                ipv6,
                device_name,
                recv_auto_accept,
                recv_msg_tx,
                recv_ctk,
            )
            .await
            {
                warn!("AirDropManager: AWDL receiver stopped: {e}");
            }
        });

        {
            let mut guard = self.inner.lock().await;
            *guard = Some(ActiveSession {
                lease_id,
                radio,
                link,
                session_ctk,
                disc_ctk: None,
            });
        }

        // Immediately start scanning for Apple devices while AirDrop is enabled
        let _ = self.start_discovery().await;
        Ok(self.probe().await)
    }

    /// Stops AirDrop receiver & discovery and releases the AWDL lease (restoring Wi-Fi).
    pub async fn disable(&self) -> Result<AirDropRadioStatus> {
        let session = self.inner.lock().await.take();
        if let Some(mut s) = session {
            if let Some(d) = s.disc_ctk.take() {
                d.cancel();
            }
            s.session_ctk.cancel();
            info!("AirDropManager: releasing AWDL lease #{}...", s.lease_id);
            if let Err(e) = s.radio.release(s.lease_id).await {
                warn!("AirDropManager: Release({}) returned error: {e}", s.lease_id);
            }
        }
        Ok(self.probe().await)
    }

    /// Starts scanning for nearby Apple AirDrop receivers (Apple Continuity BLE wake-up
    /// + `awdl0` mDNS browse + AWDL peer `/Discover` probing on port 8770).
    /// No-op if the AWDL radio is not currently active.
    pub async fn start_discovery(&self) -> Result<()> {
        let (radio, link, old_disc) = {
            let mut guard = self.inner.lock().await;
            let Some(sess) = guard.as_mut() else {
                return Ok(());
            };
            let new_disc = sess.session_ctk.child_token();
            let old = sess.disc_ctk.replace(new_disc.clone());
            (sess.radio.clone(), sess.link.clone(), (old, new_disc))
        };

        if let Some(old) = old_disc.0 {
            old.cancel();
        }
        let disc_ctk = old_disc.1;
        let device_name = self.device_name.lock().await.clone();
        let disc_tx = self.disc_tx.clone();
        let peer_flags = self.peer_flags.clone();

        tokio::spawn(async move {
            if let Err(e) =
                run_awdl_discovery(radio, link, device_name, disc_tx, peer_flags, disc_ctk).await
            {
                debug!("AirDropManager: discovery ended: {e}");
            }
        });
        Ok(())
    }

    /// Stops active AirDrop scanning (and unregisters the Apple BLE wake-up advert),
    /// keeping the receiver active if the AWDL lease is still held.
    pub async fn stop_discovery(&self) {
        let mut guard = self.inner.lock().await;
        if let Some(sess) = guard.as_mut() {
            if let Some(d) = sess.disc_ctk.take() {
                d.cancel();
            }
        }
    }

    /// Sends files to a discovered Apple AirDrop peer (`airdrop://<ipv6>%<ifindex>:<port>`).
    pub async fn send_files(
        &self,
        endpoint_id: String,
        peer_name: String,
        files: Vec<PathBuf>,
    ) -> Result<()> {
        if self.inner.lock().await.is_none() {
            return Err(anyhow!(
                "AirDrop AWDL radio is not active — enable AirDrop mode first"
            ));
        }

        let addr = parse_airdrop_endpoint_id(&endpoint_id)?;
        let flags = self
            .peer_flags
            .lock()
            .await
            .get(&endpoint_id)
            .copied()
            .unwrap_or(RECEIVER_FLAGS | SUPPORTS_DVZIP);

        let device_name = self.device_name.lock().await.clone();
        let service_id = random_service_id()?;
        let sender = Sender::new(sender::Config::new(device_name, service_id))
            .map_err(|e| anyhow!("AirDrop sender init failed: {e}"))?;

        let transfer_id = format!("airdrop-out-{}", rand_u64());
        let file_names: Vec<String> = files
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".into())
            })
            .collect();
        let total_size: u64 = files
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
            .sum();

        let emit = |state: TransferState, ack_bytes: u64, total_bytes: u64| {
            let _ = self.msg_tx.send(ChannelMessage {
                id: transfer_id.clone(),
                msg: Message::Client(MessageClient {
                    state: Some(state),
                    kind: TransferKind::Outbound,
                    metadata: Some(TransferMetadata {
                        id: transfer_id.clone(),
                        source: Some(RemoteDeviceInfo {
                            name: peer_name.clone(),
                            device_type: DeviceType::Phone,
                        }),
                        payload_kind: TransferPayloadKind::Files,
                        payload: Some(TransferPayload::Files(file_names.clone())),
                        total_bytes,
                        ack_bytes,
                        ..Default::default()
                    }),
                }),
            });
        };

        emit(TransferState::WaitingForUserConsent, 0, total_size);

        // Keep Apple BLE wake-up active during send in case the receiver needs it
        let _ble_guard = start_apple_ble_advert().await;

        let abs_files: Vec<PathBuf> = files
            .into_iter()
            .map(|p| std::path::absolute(&p).unwrap_or(p))
            .collect();
        let payload = Payload::Files(abs_files);

        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let msg_tx_prog = self.msg_tx.clone();
        let tid_prog = transfer_id.clone();
        let pname_prog = peer_name.clone();
        let fnames_prog = file_names.clone();

        let progress_task = tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                let (state, ack, tot) = match ev {
                    SenderEvent::Accepted => {
                        info!("AirDropManager: {pname_prog} accepted transfer {tid_prog}");
                        (TransferState::SendingFiles, 0, total_size)
                    }
                    SenderEvent::Progress(p) => (TransferState::SendingFiles, p.sent, p.total),
                };
                let _ = msg_tx_prog.send(ChannelMessage {
                    id: tid_prog.clone(),
                    msg: Message::Client(MessageClient {
                        state: Some(state),
                        kind: TransferKind::Outbound,
                        metadata: Some(TransferMetadata {
                            id: tid_prog.clone(),
                            source: Some(RemoteDeviceInfo {
                                name: pname_prog.clone(),
                                device_type: DeviceType::Phone,
                            }),
                            payload_kind: TransferPayloadKind::Files,
                            payload: Some(TransferPayload::Files(fnames_prog.clone())),
                            total_bytes: tot,
                            ack_bytes: ack,
                            ..Default::default()
                        }),
                    }),
                });
            }
        });

        info!(
            "AirDropManager: sending {} file(s) ({total_size} B) to {} ({addr}, flags={flags:#x})",
            file_names.len(),
            peer_name
        );
        let outcome = sender.send(addr, &payload, flags, Some(ev_tx)).await;
        let _ = progress_task.await;

        match outcome {
            Ok(Outcome::Delivered) => {
                info!("AirDropManager: transfer {transfer_id} delivered to {peer_name}");
                emit(TransferState::Finished, total_size, total_size);
                Ok(())
            }
            Ok(Outcome::Declined) => {
                info!("AirDropManager: transfer {transfer_id} declined by {peer_name}");
                emit(TransferState::Rejected, 0, total_size);
                Ok(())
            }
            Ok(Outcome::Failed(reason)) => {
                warn!("AirDropManager: transfer {transfer_id} failed: {reason}");
                emit(TransferState::Cancelled, 0, total_size);
                Err(anyhow!("AirDrop transfer failed: {reason}"))
            }
            Err(e) => {
                warn!("AirDropManager: transfer {transfer_id} error: {e}");
                emit(TransferState::Disconnected, 0, total_size);
                Err(anyhow!("AirDrop transfer error: {e}"))
            }
        }
    }
}

async fn probe_owlyd() -> Result<AirDropRadioStatus> {
    let conn = api::connect(None)
        .await
        .map_err(|e| anyhow!("AWDL radio service not reachable ({e})"))?;
    let radio = api::radio(&conn)
        .await
        .map_err(|e| anyhow!("AWDL radio proxy error ({e})"))?;
    let json = radio
        .probe()
        .await
        .map_err(|e| anyhow!("AWDL radio service not installed or inactive ({e})"))?;

    let report: ProbeReport = serde_json::from_str(&json)
        .map_err(|e| anyhow!("Failed to parse AWDL probe JSON: {e}"))?;

    let mode_raw = report.mode.as_deref().unwrap_or("unsupported").to_lowercase();
    let mode = if !report.hardware || !report.missing.is_empty() {
        "unsupported".to_string()
    } else if mode_raw.contains("shared") {
        "shared".to_string()
    } else if mode_raw.contains("exclusive") || mode_raw.contains("active") {
        "exclusive".to_string()
    } else {
        mode_raw
    };

    let available = report.hardware && report.missing.is_empty();
    let reason = report.missing.first().map(|m| m.say.clone());

    Ok(AirDropRadioStatus {
        available,
        active: false,
        mode,
        driver: report.driver,
        device: report.device,
        channel: report.channel,
        reason,
    })
}

async fn run_awdl_receiver(
    link: LinkInfo,
    ipv6: Ipv6Addr,
    device_name: String,
    auto_accept: Arc<AtomicBool>,
    msg_tx: broadcast::Sender<ChannelMessage>,
    ctk: CancellationToken,
) -> Result<()> {
    let state_dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("aethershare-airdrop");
    let identity = Identity::load_or_generate(&state_dir)
        .map_err(|e| anyhow!("Failed to load AirDrop TLS identity: {e}"))?;

    let mut config = ReceiverConfig::new(device_name.clone());
    config.model_name = "Linux".into();

    let dest_dir = get_download_dir();
    let bind_addr = SocketAddrV6::new(ipv6, LOCAL_AIRDROP_PORT, 0, link.index);

    let (receiver, mut events) =
        Receiver::bind(bind_addr, &identity, ReceiverOptions::new(config, dest_dir.clone()))
            .await
            .map_err(|e| anyhow!("Failed to bind AirDrop receiver on {bind_addr}: {e}"))?;

    let recv_task = tokio::spawn(receiver.run());

    let service_id = random_service_id()?;
    let burst = link.announce.map(|a: Announce| mdns::Burst {
        count: a.count,
        interval: a.interval,
        pause: a.pause,
    });
    let service = MdnsService {
        instance: service_id.clone(),
        host: format!("aethershare-{service_id}"),
        port: LOCAL_AIRDROP_PORT,
        addr: ipv6,
        flags: RECEIVER_FLAGS,
    };

    let _mdns_reg = Mdns::new(link.index)
        .with_burst(burst)
        .register(service)
        .await
        .map_err(|e| anyhow!("Failed to register AWDL mDNS responder: {e}"))?;

    info!(
        "AirDropManager: listening on [{ipv6}%{}]:{LOCAL_AIRDROP_PORT} as {:?} ({service_id})",
        link.interface, device_name
    );

    // Track metadata & live staging-progress pollers per active inbound transfer id
    let mut meta_map: HashMap<u64, (String, String, Vec<String>, u64)> = HashMap::new();
    let pollers: Arc<Mutex<HashMap<u64, CancellationToken>>> =
        Arc::new(Mutex::new(HashMap::new()));

    loop {
        let ev = tokio::select! {
            _ = ctk.cancelled() => break,
            ev = events.recv() => ev,
        };
        let Some(ev) = ev else {
            break;
        };

        match ev {
            ReceiverEvent::Discovered { peer, .. } => {
                debug!("AirDropManager: discovered by {peer}");
            }
            ReceiverEvent::Request(request) => {
                let req_id = request.id;
                let tid = format!("airdrop-in-{req_id}");
                let sender_name = if request.ask.sender_computer_name.trim().is_empty() {
                    "Apple Device".to_string()
                } else {
                    request.ask.sender_computer_name.clone()
                };

                let (files, total_bytes) = match &request.ask.offer {
                    Offer::Files(list) => {
                        let names: Vec<String> = list.iter().map(|f| f.name.clone()).collect();
                        let total: u64 = list.iter().filter_map(|f| f.size).sum();
                        (names, total)
                    }
                    Offer::Urls(urls) => (urls.clone(), 0),
                };

                meta_map.insert(req_id, (tid.clone(), sender_name.clone(), files.clone(), total_bytes));

                if auto_accept.load(Ordering::Relaxed) {
                    let _ = msg_tx.send(ChannelMessage {
                        id: tid.clone(),
                        msg: Message::Client(MessageClient {
                            state: Some(TransferState::ReceivingFiles),
                            kind: TransferKind::Inbound,
                            metadata: Some(TransferMetadata {
                                id: tid.clone(),
                                source: Some(RemoteDeviceInfo {
                                    name: sender_name.clone(),
                                    device_type: DeviceType::Phone,
                                }),
                                payload_kind: TransferPayloadKind::Files,
                                payload: Some(TransferPayload::Files(files.clone())),
                                total_bytes,
                                ..Default::default()
                            }),
                        }),
                    });
                    let poll_ctk = ctk.child_token();
                    pollers.lock().await.insert(req_id, poll_ctk.clone());
                    spawn_staging_progress_poller(
                        dest_dir.clone(),
                        tid,
                        sender_name,
                        files,
                        total_bytes,
                        msg_tx.clone(),
                        poll_ctk,
                    );
                    request.accept();
                } else {
                    let _ = msg_tx.send(ChannelMessage {
                        id: tid.clone(),
                        msg: Message::Client(MessageClient {
                            state: Some(TransferState::WaitingForUserConsent),
                            kind: TransferKind::Inbound,
                            metadata: Some(TransferMetadata {
                                id: tid.clone(),
                                source: Some(RemoteDeviceInfo {
                                    name: sender_name.clone(),
                                    device_type: DeviceType::Phone,
                                }),
                                payload_kind: TransferPayloadKind::Files,
                                payload: Some(TransferPayload::Files(files.clone())),
                                total_bytes,
                                ..Default::default()
                            }),
                        }),
                    });

                    // Wait asynchronously for UI / KDE notification Accept or Decline
                    let mut rx = msg_tx.subscribe();
                    let tx_reply = msg_tx.clone();
                    let pollers_clone = pollers.clone();
                    let poll_parent_ctk = ctk.clone();
                    let dest_for_poll = dest_dir.clone();
                    tokio::spawn(async move {
                        let decision = tokio::time::timeout(Duration::from_secs(60), async {
                            while let Ok(cm) = rx.recv().await {
                                if cm.id == tid {
                                    if let Message::Lib { action } = cm.msg {
                                        return action;
                                    }
                                }
                            }
                            TransferAction::ConsentDecline
                        })
                        .await;

                        match decision {
                            Ok(TransferAction::ConsentAccept) => {
                                let _ = tx_reply.send(ChannelMessage {
                                    id: tid.clone(),
                                    msg: Message::Client(MessageClient {
                                        state: Some(TransferState::ReceivingFiles),
                                        kind: TransferKind::Inbound,
                                        metadata: Some(TransferMetadata {
                                            id: tid.clone(),
                                            source: Some(RemoteDeviceInfo {
                                                name: sender_name.clone(),
                                                device_type: DeviceType::Phone,
                                            }),
                                            payload_kind: TransferPayloadKind::Files,
                                            payload: Some(TransferPayload::Files(files.clone())),
                                            total_bytes,
                                            ..Default::default()
                                        }),
                                    }),
                                });
                                let poll_ctk = poll_parent_ctk.child_token();
                                pollers_clone.lock().await.insert(req_id, poll_ctk.clone());
                                spawn_staging_progress_poller(
                                    dest_for_poll,
                                    tid,
                                    sender_name,
                                    files,
                                    total_bytes,
                                    tx_reply,
                                    poll_ctk,
                                );
                                request.accept();
                            }
                            _ => {
                                let _ = tx_reply.send(ChannelMessage {
                                    id: tid.clone(),
                                    msg: Message::Client(MessageClient {
                                        state: Some(TransferState::Rejected),
                                        kind: TransferKind::Inbound,
                                        metadata: Some(TransferMetadata {
                                            id: tid,
                                            source: Some(RemoteDeviceInfo {
                                                name: sender_name,
                                                device_type: DeviceType::Phone,
                                            }),
                                            payload_kind: TransferPayloadKind::Files,
                                            payload: Some(TransferPayload::Files(files)),
                                            total_bytes,
                                            ..Default::default()
                                        }),
                                    }),
                                });
                                request.decline();
                            }
                        }
                    });
                }
            }
            ReceiverEvent::Withdrawn { id } => {
                if let Some(p) = pollers.lock().await.remove(&id) {
                    p.cancel();
                }
                if let Some((tid, sender_name, files, total_bytes)) = meta_map.remove(&id) {
                    let _ = msg_tx.send(ChannelMessage {
                        id: tid.clone(),
                        msg: Message::Client(MessageClient {
                            state: Some(TransferState::Cancelled),
                            kind: TransferKind::Inbound,
                            metadata: Some(TransferMetadata {
                                id: tid,
                                source: Some(RemoteDeviceInfo {
                                    name: sender_name,
                                    device_type: DeviceType::Phone,
                                }),
                                payload_kind: TransferPayloadKind::Files,
                                payload: Some(TransferPayload::Files(files)),
                                total_bytes,
                                ..Default::default()
                            }),
                        }),
                    });
                }
            }
            ReceiverEvent::Received { id, paths } => {
                if let Some(p) = pollers.lock().await.remove(&id) {
                    p.cancel();
                }
                let saved: Vec<String> = paths
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                let actual_total: u64 = paths
                    .iter()
                    .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
                    .sum();
                let (tid, sender_name, files, expected_total) = meta_map
                    .remove(&id)
                    .unwrap_or_else(|| (format!("airdrop-in-{id}"), "Apple Device".into(), vec![], 0));
                let final_total = expected_total.max(actual_total);
                let _ = msg_tx.send(ChannelMessage {
                    id: tid.clone(),
                    msg: Message::Client(MessageClient {
                        state: Some(TransferState::Finished),
                        kind: TransferKind::Inbound,
                        metadata: Some(TransferMetadata {
                            id: tid,
                            source: Some(RemoteDeviceInfo {
                                name: sender_name,
                                device_type: DeviceType::Phone,
                            }),
                            payload_kind: TransferPayloadKind::Files,
                            payload: Some(TransferPayload::Files(files)),
                            destination: Some(dest_dir.to_string_lossy().to_string()),
                            saved_paths: Some(saved),
                            total_bytes: final_total,
                            ack_bytes: final_total,
                            ..Default::default()
                        }),
                    }),
                });
            }
            ReceiverEvent::Urls { id, urls } => {
                if let Some(p) = pollers.lock().await.remove(&id) {
                    p.cancel();
                }
                let (tid, sender_name, _, _) = meta_map
                    .remove(&id)
                    .unwrap_or_else(|| (format!("airdrop-in-{id}"), "Apple Device".into(), vec![], 0));
                let _ = msg_tx.send(ChannelMessage {
                    id: tid.clone(),
                    msg: Message::Client(MessageClient {
                        state: Some(TransferState::Finished),
                        kind: TransferKind::Inbound,
                        metadata: Some(TransferMetadata {
                            id: tid,
                            source: Some(RemoteDeviceInfo {
                                name: sender_name,
                                device_type: DeviceType::Phone,
                            }),
                            payload_kind: TransferPayloadKind::Text,
                            payload: Some(TransferPayload::Text(urls.join("\n"))),
                            ..Default::default()
                        }),
                    }),
                });
            }
            ReceiverEvent::Failed { id, reason } => {
                if let Some(p) = pollers.lock().await.remove(&id) {
                    p.cancel();
                }
                warn!("AirDropManager: inbound transfer {id} failed: {reason}");
                if let Some((tid, sender_name, files, total_bytes)) = meta_map.remove(&id) {
                    let _ = msg_tx.send(ChannelMessage {
                        id: tid.clone(),
                        msg: Message::Client(MessageClient {
                            state: Some(TransferState::Disconnected),
                            kind: TransferKind::Inbound,
                            metadata: Some(TransferMetadata {
                                id: tid,
                                source: Some(RemoteDeviceInfo {
                                    name: sender_name,
                                    device_type: DeviceType::Phone,
                                }),
                                payload_kind: TransferPayloadKind::Files,
                                payload: Some(TransferPayload::Files(files)),
                                total_bytes,
                                ..Default::default()
                            }),
                        }),
                    });
                }
            }
        }
    }

    recv_task.abort();
    Ok(())
}

/// Polls `owly-airdrop`'s live `.owly-staging-*` extraction directories inside `dest_dir`
/// every 350 ms so both the UI and KDE `JobViewV2` show smooth inbound byte progress.
fn spawn_staging_progress_poller(
    dest_dir: PathBuf,
    tid: String,
    sender_name: String,
    files: Vec<String>,
    total_bytes: u64,
    msg_tx: broadcast::Sender<ChannelMessage>,
    ctk: CancellationToken,
) {
    tokio::spawn(async move {
        let mut last_bytes: u64 = 0;
        loop {
            tokio::select! {
                _ = ctk.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(350)) => {}
            }
            let dir = dest_dir.clone();
            let written = tokio::task::spawn_blocking(move || sum_owly_staging_bytes(&dir))
                .await
                .unwrap_or(0);
            if written > last_bytes {
                last_bytes = written;
                let clamped = if total_bytes > 0 {
                    written.min(total_bytes.saturating_sub(1))
                } else {
                    written
                };
                let _ = msg_tx.send(ChannelMessage {
                    id: tid.clone(),
                    msg: Message::Client(MessageClient {
                        state: Some(TransferState::ReceivingFiles),
                        kind: TransferKind::Inbound,
                        metadata: Some(TransferMetadata {
                            id: tid.clone(),
                            source: Some(RemoteDeviceInfo {
                                name: sender_name.clone(),
                                device_type: DeviceType::Phone,
                            }),
                            payload_kind: TransferPayloadKind::Files,
                            payload: Some(TransferPayload::Files(files.clone())),
                            total_bytes,
                            ack_bytes: clamped,
                            ..Default::default()
                        }),
                    }),
                });
            }
        }
    });
}

fn sum_owly_staging_bytes(dest_dir: &std::path::Path) -> u64 {
    fn walk_size(path: &std::path::Path) -> u64 {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return 0;
        };
        if meta.is_file() {
            return meta.len();
        }
        if meta.is_dir() {
            let Ok(rd) = std::fs::read_dir(path) else {
                return 0;
            };
            return rd
                .flatten()
                .map(|e| walk_size(&e.path()))
                .sum();
        }
        0
    }

    let Ok(rd) = std::fs::read_dir(dest_dir) else {
        return 0;
    };
    rd.flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with(".owly-staging-"))
                .unwrap_or(false)
        })
        .map(|e| walk_size(&e.path()))
        .sum()
}

async fn run_awdl_discovery(
    radio: RadioProxy<'static>,
    link: LinkInfo,
    device_name: String,
    disc_tx: broadcast::Sender<EndpointInfo>,
    peer_flags: Arc<Mutex<HashMap<String, u32>>>,
    ctk: CancellationToken,
) -> Result<()> {
    // BLE-1: Advertise Apple Continuity AirDrop wake-up beacon so nearby iPhones/Macs
    // open their AirDrop TLS port on AWDL.
    let _ble_guard = start_apple_ble_advert().await;

    let sender = Arc::new(
        Sender::new(sender::Config::new(device_name, random_service_id()?))
            .map_err(|e| anyhow!("AirDrop sender init failed: {e}"))?,
    );

    let seen_addrs: Arc<Mutex<HashSet<SocketAddrV6>>> = Arc::new(Mutex::new(HashSet::new()));

    // Task A: Browse `_airdrop._tcp.local.` over `awdl0`
    let mdns_ctk = ctk.child_token();
    let mdns_sender = sender.clone();
    let mdns_tx = disc_tx.clone();
    let mdns_flags = peer_flags.clone();
    let mdns_seen = seen_addrs.clone();
    let if_index = link.index;
    let mdns_task = tokio::spawn(async move {
        let Ok(mut browse) = Mdns::new(if_index).browse().await else {
            return;
        };
        loop {
            let ev = tokio::select! {
                _ = mdns_ctk.cancelled() => break,
                ev = browse.events.recv() => ev,
            };
            let Some(BrowseEvent::Found(found)) = ev else {
                continue;
            };
            let addr = SocketAddrV6::new(found.addr, found.port, 0, if_index);
            if !mdns_seen.lock().await.insert(addr) {
                continue;
            }
            if let Ok(Ok(Some(rec))) =
                tokio::time::timeout(Duration::from_secs(4), mdns_sender.discover(addr)).await
            {
                emit_discovered_peer(
                    addr,
                    rec.computer_name,
                    found.flags,
                    &mdns_tx,
                    &mdns_flags,
                )
                .await;
            } else {
                mdns_seen.lock().await.remove(&addr);
            }
        }
    });

    // Task B: Probe AWDL peers reported by `aethershare-awdld` directly on port 8770 (DISC-5),
    // because iPhones often do not multicast mDNS across `awdl0`.
    let peers_ctk = ctk.child_token();
    let peers_sender = sender.clone();
    let peers_tx = disc_tx.clone();
    let peers_flags = peer_flags.clone();
    let peers_seen = seen_addrs.clone();
    let peers_task = tokio::spawn(async move {
        while !peers_ctk.is_cancelled() {
            let raw_peers = radio.peers().await.unwrap_or_default();
            for p in raw_peers {
                if peers_ctk.is_cancelled() {
                    break;
                }
                let Some(mac_str) = p.get("mac").and_then(|v| v.downcast_ref::<String>().ok())
                else {
                    continue;
                };
                let Ok(mac) = mac_str.parse::<MacAddr>() else {
                    continue;
                };
                let addr = SocketAddrV6::new(mac.link_local(), APPLE_AIRDROP_PORT, 0, if_index);
                if peers_seen.lock().await.contains(&addr) {
                    continue;
                }
                if let Ok(Ok(Some(rec))) =
                    tokio::time::timeout(Duration::from_secs(3), peers_sender.discover(addr)).await
                {
                    if peers_seen.lock().await.insert(addr) {
                        emit_discovered_peer(
                            addr,
                            rec.computer_name,
                            RECEIVER_FLAGS | SUPPORTS_DVZIP,
                            &peers_tx,
                            &peers_flags,
                        )
                        .await;
                    }
                }
            }
            tokio::select! {
                _ = peers_ctk.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(800)) => {}
            }
        }
    });

    ctk.cancelled().await;
    mdns_task.abort();
    peers_task.abort();
    Ok(())
}

async fn emit_discovered_peer(
    addr: SocketAddrV6,
    name: String,
    flags: u32,
    disc_tx: &broadcast::Sender<EndpointInfo>,
    peer_flags: &Arc<Mutex<HashMap<String, u32>>>,
) {
    let id = format!("airdrop://[{}%{}]:{}", addr.ip(), addr.scope_id(), addr.port());
    peer_flags.lock().await.insert(id.clone(), flags);

    let rtype = {
        let lower = name.to_lowercase();
        if lower.contains("mac") {
            DeviceType::Laptop
        } else if lower.contains("ipad") {
            DeviceType::Tablet
        } else {
            DeviceType::Phone
        }
    };

    let ei = EndpointInfo {
        fullname: id.clone(),
        id,
        name: Some(name),
        ip: Some(format!("{}%{}", addr.ip(), addr.scope_id())),
        port: Some(addr.port().to_string()),
        rtype: Some(rtype),
        present: Some(true),
        ble_addr: None,
        ble_psm: None,
        protocol: Some("airdrop".into()),
    };
    info!("AirDropManager: found Apple AirDrop peer {:?}", ei);
    let _ = disc_tx.send(ei);
}

fn parse_airdrop_endpoint_id(id: &str) -> Result<SocketAddrV6> {
    let rest = id
        .strip_prefix("airdrop://")
        .ok_or_else(|| anyhow!("Not an airdrop endpoint id: {id}"))?;
    // Expected format: [fe80::...%<scope_id>]:<port>
    let rest = rest
        .strip_prefix('[')
        .ok_or_else(|| anyhow!("Malformed AWDL AirDrop endpoint id: {id}"))?;
    let (addr_part, port_str) = rest
        .split_once("]:")
        .ok_or_else(|| anyhow!("Missing port in AWDL AirDrop endpoint id: {id}"))?;
    let (ip_str, scope_str) = addr_part
        .split_once('%')
        .ok_or_else(|| anyhow!("Missing scope_id in AWDL AirDrop endpoint id: {id}"))?;
    let ip: Ipv6Addr = ip_str.parse()?;
    let scope_id: u32 = scope_str.parse()?;
    let port: u16 = port_str.parse()?;
    Ok(SocketAddrV6::new(ip, port, 0, scope_id))
}

fn random_service_id() -> Result<String> {
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("getrandom failed: {e}"))?;
    Ok(mdns::service_id(bytes))
}

fn rand_u64() -> u64 {
    let mut bytes = [0u8; 8];
    let _ = getrandom::fill(&mut bytes);
    u64::from_le_bytes(bytes)
}

// =============================================================================
// Apple Continuity BLE Wake-Up Advertisement over BlueZ (`0x004C`, AirDrop TLV)
// =============================================================================

const BLUEZ_LE_ADV_MGR: &str = "org.bluez.LEAdvertisingManager1";
static BLE_ADV_SEQ: AtomicU32 = AtomicU32::new(0);

struct AppleBleAdvertisement {
    data: Vec<u8>,
}

#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl AppleBleAdvertisement {
    fn release(&self) {}

    #[zbus(property, name = "Type")]
    fn kind(&self) -> &str {
        "peripheral"
    }

    #[zbus(property)]
    fn min_interval(&self) -> u32 {
        INTERVAL_MS
    }

    #[zbus(property)]
    fn max_interval(&self) -> u32 {
        INTERVAL_MS
    }

    #[zbus(property)]
    fn manufacturer_data(&self) -> HashMap<u16, Value<'_>> {
        HashMap::from([(APPLE_COMPANY_ID, Value::from(self.data.clone()))])
    }
}

struct BleAdvHandle {
    conn: zbus::Connection,
    adapter: OwnedObjectPath,
    path: OwnedObjectPath,
}

impl BleAdvHandle {
    async fn unregister(self) {
        let _ = self
            .conn
            .call_method(
                Some("org.bluez"),
                self.adapter.as_str(),
                Some(BLUEZ_LE_ADV_MGR),
                "UnregisterAdvertisement",
                &(&self.path,),
            )
            .await;
        let _ = self
            .conn
            .object_server()
            .remove::<AppleBleAdvertisement, _>(&self.path)
            .await;
    }
}

struct BleAdvGuard(Option<BleAdvHandle>);

impl Drop for BleAdvGuard {
    fn drop(&mut self) {
        if let (Some(handle), Ok(rt)) = (self.0.take(), tokio::runtime::Handle::try_current()) {
            rt.spawn(handle.unregister());
        }
    }
}

async fn start_apple_ble_advert() -> Option<BleAdvGuard> {
    match register_apple_ble_advert().await {
        Ok(guard) => {
            debug!("AirDropManager: Apple Continuity BLE wake-up advert active");
            Some(guard)
        }
        Err(e) => {
            warn!("AirDropManager: Apple BLE wake-up advert unavailable: {e}");
            None
        }
    }
}

async fn register_apple_ble_advert() -> io::Result<BleAdvGuard> {
    let conn = zbus::Connection::system().await.map_err(io::Error::other)?;
    let objects = zbus::fdo::ObjectManagerProxy::builder(&conn)
        .destination("org.bluez")
        .and_then(|b| b.path("/"))
        .map_err(io::Error::other)?
        .build()
        .await
        .map_err(io::Error::other)?
        .get_managed_objects()
        .await
        .map_err(io::Error::other)?;

    let mut adapters: Vec<OwnedObjectPath> = objects
        .into_iter()
        .filter(|(_, ifaces)| ifaces.keys().any(|k| k.as_str() == BLUEZ_LE_ADV_MGR))
        .map(|(path, _)| path)
        .collect();
    adapters.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let adapter = adapters
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no BlueZ LE advertising adapter"))?;

    let mut tag = [0u8; 3];
    getrandom::fill(&mut tag).map_err(|e| io::Error::other(e.to_string()))?;
    let advert = Advert::anonymous(tag);

    let seq = BLE_ADV_SEQ.fetch_add(1, Ordering::Relaxed);
    let path: OwnedObjectPath = ObjectPath::try_from(format!(
        "/org/aethershare/airdrop/ble_adv_{}_{seq}",
        std::process::id()
    ))
    .map_err(io::Error::other)?
    .into();

    conn.object_server()
        .at(
            &path,
            AppleBleAdvertisement {
                data: advert.manufacturer_data(),
            },
        )
        .await
        .map_err(io::Error::other)?;

    let options = HashMap::<&str, Value>::new();
    if let Err(e) = conn
        .call_method(
            Some("org.bluez"),
            adapter.as_str(),
            Some(BLUEZ_LE_ADV_MGR),
            "RegisterAdvertisement",
            &(&path, options),
        )
        .await
    {
        let _ = conn
            .object_server()
            .remove::<AppleBleAdvertisement, _>(&path)
            .await;
        return Err(io::Error::other(e));
    }

    Ok(BleAdvGuard(Some(BleAdvHandle {
        conn,
        adapter,
        path,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_awdl_airdrop_endpoint_id() {
        let addr = parse_airdrop_endpoint_id("airdrop://[fe80::1234:56ff:fe78:9abc%7]:8770").unwrap();
        assert_eq!(addr.port(), 8770);
        assert_eq!(addr.scope_id(), 7);
        assert_eq!(addr.ip(), &"fe80::1234:56ff:fe78:9abc".parse::<Ipv6Addr>().unwrap());
    }
}
