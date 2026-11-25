//! Versioned Local Bridge API (`/api/v1`) state management for external clients
//! (such as KDE Plasma widgets / Dynamic Islands, scripts, and secondary UIs).
//!
//! Provides:
//! - [`ApiAuth`]: Per-user local access token (`$XDG_RUNTIME_DIR/aethershare/api.token`,
//!   mode `0600`) and endpoint discovery file (`$XDG_RUNTIME_DIR/aethershare/api.json`).
//! - [`DeviceRegistry`]: Live reachable device snapshot, LAN+BLE merging into a single
//!   stable `device_id`, disambiguation of identically named devices, and automatic
//!   optimal transport selection.
//! - [`DiscoverySessionManager`]: Multi-client reference-counted discovery sessions
//!   with TTL expiration so closing a widget never stops the main app's scan (and vice versa).
//! - [`TransferRegistry`]: Idempotent `request_id` deduplication, normalized widget-friendly
//!   transfer states (`connecting`, `waiting_for_consent`, `in_progress`, `completed`,
//!   `declined`, `cancelled`, `failed`), and SSE event streaming.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use crate::channel::{ChannelMessage, Message, TransferKind};
use crate::hdl::info::TransferPayload;
use crate::{DeviceType, EndpointInfo, TransferState};

/// Default discovery session TTL when the caller does not specify one (5 minutes).
pub const DEFAULT_DISCOVERY_TTL_SECS: u64 = 300;
/// Maximum allowed discovery session TTL (1 hour).
pub const MAX_DISCOVERY_TTL_SECS: u64 = 3600;
/// Minimum allowed discovery session TTL (5 seconds).
pub const MIN_DISCOVERY_TTL_SECS: u64 = 5;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn short_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    hex::encode(&digest[..8])
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// =============================================================================
// Structured API v1 Error
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorEnvelope {
    pub error: ApiErrorDetail,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorDetail {
    pub code: &'static str,
    pub message: String,
}

pub fn api_error_json(code: &'static str, message: impl Into<String>) -> Vec<u8> {
    serde_json::to_vec(&ApiErrorEnvelope {
        error: ApiErrorDetail {
            code,
            message: message.into(),
        },
    })
    .unwrap_or_else(|_| {
        b"{\"error\":{\"code\":\"INTERNAL_ERROR\",\"message\":\"Serialization error\"}}".to_vec()
    })
}

// =============================================================================
// Local Access Token & Discovery Metadata (`$XDG_RUNTIME_DIR/aethershare/`)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiDiscoveryInfo {
    pub url: String,
    pub api_base: String,
    pub token_file: String,
    pub version: String,
    pub pid: u32,
}

#[derive(Debug, Clone)]
pub struct ApiAuth {
    token: String,
    token_path: PathBuf,
    info_path: PathBuf,
    base_url: String,
}

impl ApiAuth {
    /// Initializes (or reuses) the per-user local API token at
    /// `$XDG_RUNTIME_DIR/aethershare/api.token` (mode `0600`) and writes
    /// `$XDG_RUNTIME_DIR/aethershare/api.json`.
    pub fn init(bridge_port: u16) -> Self {
        let dir = runtime_dir();
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }

        let token_path = dir.join("api.token");
        let info_path = dir.join("api.json");
        let token = std::fs::read_to_string(&token_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() >= 32)
            .unwrap_or_else(|| random_hex(32));

        write_private_file(&token_path, format!("{token}\n").as_bytes());

        let base_url = format!("http://127.0.0.1:{bridge_port}");
        let info = ApiDiscoveryInfo {
            url: base_url.clone(),
            api_base: format!("{base_url}/api/v1"),
            token_file: token_path.to_string_lossy().to_string(),
            version: crate::APP_VERSION.to_string(),
            pid: std::process::id(),
        };
        if let Ok(json) = serde_json::to_vec_pretty(&info) {
            write_private_file(&info_path, &json);
        }

        Self {
            token,
            token_path,
            info_path,
            base_url,
        }
    }

    pub fn token_path(&self) -> &Path {
        &self.token_path
    }

    pub fn info_path(&self) -> &Path {
        &self.info_path
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Verifies `Authorization: Bearer <token>` or `?token=<token>`.
    pub fn verify(&self, auth_header: Option<&str>, query_token: Option<&str>) -> bool {
        if let Some(hdr) = auth_header {
            let trimmed = hdr.trim();
            if let Some(bearer) = trimmed
                .strip_prefix("Bearer ")
                .or_else(|| trimmed.strip_prefix("bearer "))
            {
                if constant_time_eq(bearer.trim().as_bytes(), self.token.as_bytes()) {
                    return true;
                }
            }
        }
        if let Some(qt) = query_token {
            if constant_time_eq(qt.trim().as_bytes(), self.token.as_bytes()) {
                return true;
            }
        }
        false
    }
}

fn runtime_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg);
        if !p.as_os_str().is_empty() {
            return p.join("aethershare");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/aethershare");
    }
    PathBuf::from("/tmp/aethershare")
}

fn write_private_file(path: &Path, content: &[u8]) {
    if std::fs::write(path, content).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

// =============================================================================
// Device Registry (Stable IDs + LAN/BLE Merge + Same-Name Disambiguation)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiDevice {
    pub device_id: String,
    pub name: String,
    pub disambiguator: Option<String>,
    pub device_type: String,
    pub protocol: String,
    pub transports: Vec<String>,
    pub preferred_transport: Option<String>,
    pub reachable: bool,
    pub last_seen_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiDeviceEvent {
    #[serde(rename = "type")]
    pub event_type: String, // "upsert" | "removed"
    pub device: ApiDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedSendRoute {
    QuickShareLan {
        addr: String,
        peer_name: String,
    },
    QuickShareBle {
        peer_name: String,
    },
    AirDrop {
        endpoint_id: String,
        peer_name: String,
    },
}

#[derive(Debug, Clone)]
struct LanTransport {
    endpoint_id: String,
    ip: String,
    port: String,
    present: bool,
}

#[derive(Debug, Clone)]
struct BleTransport {
    ble_addr: String,
    present: bool,
}

#[derive(Debug, Clone)]
struct AirDropTransport {
    endpoint_id: String,
    ipv6_core: String,
    present: bool,
}

#[derive(Debug, Clone)]
struct DeviceRecord {
    device_id: String,
    name: String,
    device_type: DeviceType,
    protocol: String, // "quickshare" | "airdrop"
    lan: Option<LanTransport>,
    ble: Option<BleTransport>,
    airdrop: Option<AirDropTransport>,
    last_seen_ms: u64,
}

impl DeviceRecord {
    fn is_reachable(&self) -> bool {
        self.lan.as_ref().map(|l| l.present).unwrap_or(false)
            || self.ble.as_ref().map(|b| b.present).unwrap_or(false)
            || self.airdrop.as_ref().map(|a| a.present).unwrap_or(false)
    }

    fn transports(&self) -> Vec<String> {
        let mut t = Vec::new();
        if self.lan.as_ref().map(|l| l.present).unwrap_or(false) {
            t.push("lan".to_string());
        }
        if self.ble.as_ref().map(|b| b.present).unwrap_or(false) {
            t.push("ble".to_string());
        }
        if self.airdrop.as_ref().map(|a| a.present).unwrap_or(false) {
            t.push("awdl".to_string());
        }
        t
    }

    fn preferred_transport(&self) -> Option<String> {
        if self.airdrop.as_ref().map(|a| a.present).unwrap_or(false) {
            return Some("awdl".into());
        }
        if self.lan.as_ref().map(|l| l.present).unwrap_or(false) {
            return Some("lan".into());
        }
        if self.ble.as_ref().map(|b| b.present).unwrap_or(false) {
            return Some("ble".into());
        }
        None
    }

    fn to_api(&self, needs_disambiguator: bool) -> ApiDevice {
        let disambiguator = if needs_disambiguator {
            let suffix = self
                .device_id
                .strip_prefix("dev_")
                .unwrap_or(&self.device_id);
            let short = &suffix[..suffix.len().min(4)];
            Some(format!("#{short}"))
        } else {
            None
        };
        ApiDevice {
            device_id: self.device_id.clone(),
            name: self.name.clone(),
            disambiguator,
            device_type: device_type_str(&self.device_type).to_string(),
            protocol: self.protocol.clone(),
            transports: self.transports(),
            preferred_transport: self.preferred_transport(),
            reachable: self.is_reachable(),
            last_seen_ms: self.last_seen_ms,
        }
    }
}

fn device_type_str(dt: &DeviceType) -> &'static str {
    match dt {
        DeviceType::Phone => "phone",
        DeviceType::Tablet => "tablet",
        DeviceType::Laptop => "laptop",
        DeviceType::Unknown => "unknown",
    }
}

fn extract_airdrop_ipv6_core(ei: &EndpointInfo) -> Option<String> {
    if let Some(ip) = ei.ip.as_deref() {
        let core = ip.split('%').next().unwrap_or(ip).trim();
        if !core.is_empty() {
            return Some(core.to_ascii_lowercase());
        }
    }
    if let Some(rest) = ei.id.strip_prefix("airdrop://") {
        let rest = rest.trim_start_matches('[');
        let core = rest
            .split('%')
            .next()
            .unwrap_or(rest)
            .split("]:")
            .next()
            .unwrap_or(rest)
            .trim();
        if !core.is_empty() {
            return Some(core.to_ascii_lowercase());
        }
    }
    None
}

#[derive(Clone)]
pub struct DeviceRegistry {
    records: Arc<Mutex<HashMap<String, DeviceRecord>>>,
    /// Maps raw discovery endpoint IDs (mDNS ID, `ble://...`, `airdrop://...`) -> `device_id`
    /// so removal events (`present: Some(false)`) always find the exact device.
    raw_id_index: Arc<Mutex<HashMap<String, String>>>,
    events_tx: broadcast::Sender<ApiDeviceEvent>,
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceRegistry {
    pub fn new() -> Self {
        let (events_tx, _) = broadcast::channel(128);
        Self {
            records: Arc::new(Mutex::new(HashMap::new())),
            raw_id_index: Arc::new(Mutex::new(HashMap::new())),
            events_tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ApiDeviceEvent> {
        self.events_tx.subscribe()
    }

    /// Returns an immediate snapshot of all currently reachable unified devices.
    pub fn list_reachable(&self) -> Vec<ApiDevice> {
        let guard = self.records.lock().unwrap();
        let reachable: Vec<&DeviceRecord> = guard.values().filter(|r| r.is_reachable()).collect();
        let mut name_counts: HashMap<String, usize> = HashMap::new();
        for r in &reachable {
            *name_counts.entry(r.name.trim().to_ascii_lowercase()).or_insert(0) += 1;
        }
        let mut out: Vec<ApiDevice> = reachable
            .into_iter()
            .map(|r| {
                let dup = name_counts
                    .get(&r.name.trim().to_ascii_lowercase())
                    .copied()
                    .unwrap_or(0)
                    > 1;
                r.to_api(dup)
            })
            .collect();
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.device_id.cmp(&b.device_id)));
        out
    }

    /// Resolves a stable `device_id` into the best currently reachable send transport.
    pub fn resolve_route(&self, device_id: &str) -> Option<ResolvedSendRoute> {
        let guard = self.records.lock().unwrap();
        let rec = guard.get(device_id)?;
        if let Some(ad) = &rec.airdrop {
            if ad.present {
                return Some(ResolvedSendRoute::AirDrop {
                    endpoint_id: ad.endpoint_id.clone(),
                    peer_name: rec.name.clone(),
                });
            }
        }
        if let Some(lan) = &rec.lan {
            if lan.present && !lan.ip.is_empty() && !lan.port.is_empty() {
                return Some(ResolvedSendRoute::QuickShareLan {
                    addr: format!("{}:{}", lan.ip, lan.port),
                    peer_name: rec.name.clone(),
                });
            }
        }
        if let Some(ble) = &rec.ble {
            if ble.present {
                return Some(ResolvedSendRoute::QuickShareBle {
                    peer_name: rec.name.clone(),
                });
            }
        }
        None
    }

    pub fn get_device(&self, device_id: &str) -> Option<ApiDevice> {
        let guard = self.records.lock().unwrap();
        let rec = guard.get(device_id)?;
        let norm = rec.name.trim().to_ascii_lowercase();
        let dup = guard
            .values()
            .filter(|r| r.is_reachable() && r.name.trim().to_ascii_lowercase() == norm)
            .count()
            > 1;
        Some(rec.to_api(dup))
    }

    /// Ingests a raw discovery `EndpointInfo` (Quick Share LAN, Quick Share BLE, or AirDrop AWDL),
    /// merges transports of the same physical device, keeps same-named devices distinct,
    /// and broadcasts normalized `ApiDeviceEvent`s.
    pub fn ingest(&self, ei: &EndpointInfo) -> Option<ApiDeviceEvent> {
        let present = ei.present.unwrap_or(false);
        let is_airdrop = ei.protocol.as_deref() == Some("airdrop") || ei.id.starts_with("airdrop://");
        let is_ble = !is_airdrop && (ei.ble_addr.is_some() || ei.id.starts_with("ble://"));

        if !present {
            return self.handle_removal(ei, is_airdrop, is_ble);
        }

        let name = ei.name.as_deref().unwrap_or("").trim().to_string();
        if name.is_empty() {
            return None;
        }
        let rtype = ei.rtype.clone().unwrap_or(DeviceType::Unknown);
        let ts = now_ms();

        let mut records = self.records.lock().unwrap();
        let mut raw_index = self.raw_id_index.lock().unwrap();

        let device_id = if is_airdrop {
            let ipv6_core = extract_airdrop_ipv6_core(ei).unwrap_or_else(|| ei.id.clone());
            // Match existing AirDrop record by stable link-local IPv6 (independent of awdl0 ifindex)
            let existing_id = records
                .values()
                .find(|r| {
                    r.protocol == "airdrop"
                        && r.airdrop
                            .as_ref()
                            .map(|a| a.ipv6_core == ipv6_core)
                            .unwrap_or(false)
                })
                .map(|r| r.device_id.clone());

            let dev_id = existing_id.unwrap_or_else(|| format!("dev_{}", short_hash(&format!("airdrop:{ipv6_core}"))));
            let rec = records.entry(dev_id.clone()).or_insert_with(|| DeviceRecord {
                device_id: dev_id.clone(),
                name: name.clone(),
                device_type: rtype.clone(),
                protocol: "airdrop".into(),
                lan: None,
                ble: None,
                airdrop: None,
                last_seen_ms: ts,
            });
            rec.name = name;
            if rtype != DeviceType::Unknown {
                rec.device_type = rtype;
            }
            rec.airdrop = Some(AirDropTransport {
                endpoint_id: ei.id.clone(),
                ipv6_core,
                present: true,
            });
            rec.last_seen_ms = ts;
            raw_index.insert(ei.id.clone(), dev_id.clone());
            dev_id
        } else if is_ble {
            let ble_addr = ei
                .ble_addr
                .clone()
                .unwrap_or_else(|| ei.id.clone())
                .to_ascii_lowercase();
            let norm_name = name.to_ascii_lowercase();

            // 1. Existing record with same BLE address
            let by_mac = records
                .values()
                .find(|r| {
                    r.protocol == "quickshare"
                        && r.ble
                            .as_ref()
                            .map(|b| b.ble_addr == ble_addr)
                            .unwrap_or(false)
                })
                .map(|r| r.device_id.clone());

            // 2. Otherwise merge onto a Quick Share LAN-only record with the exact same name
            //    (only if that record does NOT already have a different BLE address, so two
            //    same-named phones with different BLE MACs stay distinct!).
            let by_lan_merge = if by_mac.is_none() {
                records
                    .values()
                    .find(|r| {
                        r.protocol == "quickshare"
                            && r.lan.as_ref().map(|l| l.present).unwrap_or(false)
                            && r.ble.is_none()
                            && r.name.trim().to_ascii_lowercase() == norm_name
                    })
                    .map(|r| r.device_id.clone())
            } else {
                None
            };

            let dev_id = by_mac
                .or(by_lan_merge)
                .unwrap_or_else(|| format!("dev_{}", short_hash(&format!("qs-ble:{ble_addr}"))));

            let rec = records.entry(dev_id.clone()).or_insert_with(|| DeviceRecord {
                device_id: dev_id.clone(),
                name: name.clone(),
                device_type: rtype.clone(),
                protocol: "quickshare".into(),
                lan: None,
                ble: None,
                airdrop: None,
                last_seen_ms: ts,
            });
            rec.name = name;
            if rtype != DeviceType::Unknown {
                rec.device_type = rtype;
            }
            rec.ble = Some(BleTransport {
                ble_addr,
                present: true,
            });
            rec.last_seen_ms = ts;
            raw_index.insert(ei.id.clone(), dev_id.clone());
            dev_id
        } else {
            // Quick Share LAN (mDNS)
            let ip = ei.ip.clone().unwrap_or_default();
            let port = ei.port.clone().unwrap_or_default();
            let norm_name = name.to_ascii_lowercase();

            // 1. Existing record with same mDNS endpoint_id or same IP
            let by_lan = records
                .values()
                .find(|r| {
                    r.protocol == "quickshare"
                        && r.lan
                            .as_ref()
                            .map(|l| l.endpoint_id == ei.id || (!ip.is_empty() && l.ip == ip))
                            .unwrap_or(false)
                })
                .map(|r| r.device_id.clone());

            // 2. Otherwise merge onto a Quick Share BLE-only record with the exact same name
            //    (only if that record does NOT already have a different LAN IP/endpoint_id, so
            //    two same-named devices on LAN stay distinct!).
            let by_ble_merge = if by_lan.is_none() {
                records
                    .values()
                    .find(|r| {
                        r.protocol == "quickshare"
                            && r.ble.as_ref().map(|b| b.present).unwrap_or(false)
                            && r.lan.is_none()
                            && r.name.trim().to_ascii_lowercase() == norm_name
                    })
                    .map(|r| r.device_id.clone())
            } else {
                None
            };

            let key_seed = if !ei.id.is_empty() {
                format!("qs-lan:{}:{ip}", ei.id)
            } else {
                format!("qs-lan:{ip}:{port}")
            };
            let dev_id = by_lan
                .or(by_ble_merge)
                .unwrap_or_else(|| format!("dev_{}", short_hash(&key_seed)));

            let rec = records.entry(dev_id.clone()).or_insert_with(|| DeviceRecord {
                device_id: dev_id.clone(),
                name: name.clone(),
                device_type: rtype.clone(),
                protocol: "quickshare".into(),
                lan: None,
                ble: None,
                airdrop: None,
                last_seen_ms: ts,
            });
            rec.name = name;
            if rtype != DeviceType::Unknown {
                rec.device_type = rtype;
            }
            rec.lan = Some(LanTransport {
                endpoint_id: ei.id.clone(),
                ip,
                port,
                present: true,
            });
            rec.last_seen_ms = ts;
            raw_index.insert(ei.id.clone(), dev_id.clone());
            dev_id
        };

        let target_norm = records
            .get(&device_id)
            .map(|r| r.name.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let dup = records
            .values()
            .filter(|r| r.is_reachable() && r.name.trim().to_ascii_lowercase() == target_norm)
            .count()
            > 1;
        let api_dev = records.get(&device_id)?.to_api(dup);
        let ev = ApiDeviceEvent {
            event_type: "upsert".into(),
            device: api_dev,
        };
        let _ = self.events_tx.send(ev.clone());
        Some(ev)
    }

    fn handle_removal(
        &self,
        ei: &EndpointInfo,
        is_airdrop: bool,
        is_ble: bool,
    ) -> Option<ApiDeviceEvent> {
        let mut records = self.records.lock().unwrap();
        let raw_index = self.raw_id_index.lock().unwrap();

        let dev_id = raw_index.get(&ei.id).cloned().or_else(|| {
            if is_airdrop {
                let core = extract_airdrop_ipv6_core(ei)?;
                records
                    .values()
                    .find(|r| {
                        r.airdrop
                            .as_ref()
                            .map(|a| a.ipv6_core == core || a.endpoint_id == ei.id)
                            .unwrap_or(false)
                    })
                    .map(|r| r.device_id.clone())
            } else if is_ble {
                let mac = ei.ble_addr.as_deref().unwrap_or(&ei.id).to_ascii_lowercase();
                records
                    .values()
                    .find(|r| r.ble.as_ref().map(|b| b.ble_addr == mac).unwrap_or(false))
                    .map(|r| r.device_id.clone())
            } else {
                records
                    .values()
                    .find(|r| r.lan.as_ref().map(|l| l.endpoint_id == ei.id).unwrap_or(false))
                    .map(|r| r.device_id.clone())
            }
        })?;

        let rec = records.get_mut(&dev_id)?;
        let was_reachable = rec.is_reachable();
        if is_airdrop {
            if let Some(a) = rec.airdrop.as_mut() {
                a.present = false;
            }
        } else if is_ble {
            if let Some(b) = rec.ble.as_mut() {
                b.present = false;
            }
        } else if let Some(l) = rec.lan.as_mut() {
            l.present = false;
        }
        rec.last_seen_ms = now_ms();

        let still_reachable = rec.is_reachable();
        if !was_reachable && !still_reachable {
            return None;
        }

        let api_dev = rec.to_api(false);
        let ev = ApiDeviceEvent {
            event_type: if still_reachable {
                "upsert".into()
            } else {
                "removed".into()
            },
            device: api_dev,
        };
        let _ = self.events_tx.send(ev.clone());
        Some(ev)
    }
}

// =============================================================================
// Multi-Client Discovery Session Manager
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct DiscoverySessionInfo {
    pub session_id: String,
    pub client: String,
    pub expires_in_seconds: u64,
}

struct SessionEntry {
    client: String,
    expires_at: Instant,
}

#[derive(Clone, Default)]
pub struct DiscoverySessionManager {
    sessions: Arc<Mutex<HashMap<String, SessionEntry>>>,
}

impl DiscoverySessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new discovery session. Returns `(session_id, should_start_hardware, active_count, ttl_secs)`.
    pub fn create_session(
        &self,
        client: Option<String>,
        ttl_secs: Option<u64>,
    ) -> (String, bool, usize, u64) {
        let ttl = ttl_secs
            .unwrap_or(DEFAULT_DISCOVERY_TTL_SECS)
            .clamp(MIN_DISCOVERY_TTL_SECS, MAX_DISCOVERY_TTL_SECS);
        let session_id = format!("ds_{}", random_hex(12));
        let mut guard = self.sessions.lock().unwrap();
        let now = Instant::now();
        guard.retain(|_, v| v.expires_at > now);
        let was_empty = guard.is_empty();
        guard.insert(
            session_id.clone(),
            SessionEntry {
                client: client.unwrap_or_else(|| "external".into()),
                expires_at: now + Duration::from_secs(ttl),
            },
        );
        (session_id, was_empty, guard.len(), ttl)
    }

    /// Creates or refreshes a fixed-ID session (used by the built-in desktop UI so its
    /// periodic scan burst never cancels an external KDE widget's session).
    pub fn ensure_named_session(
        &self,
        session_id: &str,
        client: &str,
        ttl_secs: u64,
    ) -> (bool, usize) {
        let mut guard = self.sessions.lock().unwrap();
        let now = Instant::now();
        guard.retain(|_, v| v.expires_at > now);
        let was_empty = guard.is_empty();
        guard.insert(
            session_id.to_string(),
            SessionEntry {
                client: client.to_string(),
                expires_at: now + Duration::from_secs(ttl_secs),
            },
        );
        (was_empty, guard.len())
    }

    /// Extends an existing session's TTL. Returns `Some(ttl_secs)` if found.
    pub fn heartbeat(&self, session_id: &str, ttl_secs: Option<u64>) -> Option<u64> {
        let ttl = ttl_secs
            .unwrap_or(DEFAULT_DISCOVERY_TTL_SECS)
            .clamp(MIN_DISCOVERY_TTL_SECS, MAX_DISCOVERY_TTL_SECS);
        let mut guard = self.sessions.lock().unwrap();
        let now = Instant::now();
        guard.retain(|_, v| v.expires_at > now);
        let entry = guard.get_mut(session_id)?;
        entry.expires_at = now + Duration::from_secs(ttl);
        Some(ttl)
    }

    /// Removes a session. Returns `(existed, should_stop_hardware, active_count)`.
    pub fn remove_session(&self, session_id: &str) -> (bool, bool, usize) {
        let mut guard = self.sessions.lock().unwrap();
        let now = Instant::now();
        let had_active = !guard.is_empty();
        let existed = guard.remove(session_id).is_some();
        guard.retain(|_, v| v.expires_at > now);
        let now_empty = guard.is_empty();
        (existed, had_active && now_empty, guard.len())
    }

    /// Prunes expired sessions. Returns `(should_stop_hardware, active_count)`.
    pub fn prune_expired(&self) -> (bool, usize) {
        let mut guard = self.sessions.lock().unwrap();
        let now = Instant::now();
        let before = guard.len();
        guard.retain(|_, v| v.expires_at > now);
        let after = guard.len();
        (before > 0 && after == 0, after)
    }

    pub fn active_count(&self) -> usize {
        let mut guard = self.sessions.lock().unwrap();
        let now = Instant::now();
        guard.retain(|_, v| v.expires_at > now);
        guard.len()
    }

    pub fn list(&self) -> Vec<DiscoverySessionInfo> {
        let mut guard = self.sessions.lock().unwrap();
        let now = Instant::now();
        guard.retain(|_, v| v.expires_at > now);
        let mut out: Vec<DiscoverySessionInfo> = guard
            .iter()
            .map(|(id, e)| DiscoverySessionInfo {
                session_id: id.clone(),
                client: e.client.clone(),
                expires_in_seconds: e.expires_at.saturating_duration_since(now).as_secs(),
            })
            .collect();
        out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        out
    }
}

// =============================================================================
// Transfer Registry (Idempotent `request_id` + Normalized Widget Transfer Events)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiTransfer {
    pub transfer_id: String,
    pub request_id: Option<String>,
    pub direction: String, // "outbound" | "inbound"
    pub protocol: String,  // "quickshare" | "airdrop"
    pub device_id: Option<String>,
    pub device_name: String,
    /// Normalized widget state:
    /// `"connecting"` | `"waiting_for_consent"` | `"in_progress"` |
    /// `"completed"` | `"declined"` | `"cancelled"` | `"failed"`
    pub state: String,
    pub pin_code: Option<String>,
    pub files: Vec<String>,
    pub saved_paths: Vec<String>,
    pub bytes_transferred: u64,
    pub total_bytes: u64,
    pub progress: f64,
    pub error: Option<String>,
    pub updated_at_ms: u64,
}

impl ApiTransfer {
    fn is_terminal(&self) -> bool {
        matches!(
            self.state.as_str(),
            "completed" | "declined" | "cancelled" | "failed"
        )
    }
}

#[derive(Clone)]
pub struct TransferRegistry {
    transfers: Arc<Mutex<HashMap<String, ApiTransfer>>>,
    request_ids: Arc<Mutex<HashMap<String, String>>>,
    events_tx: broadcast::Sender<ApiTransfer>,
}

impl Default for TransferRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TransferRegistry {
    pub fn new() -> Self {
        let (events_tx, _) = broadcast::channel(128);
        Self {
            transfers: Arc::new(Mutex::new(HashMap::new())),
            request_ids: Arc::new(Mutex::new(HashMap::new())),
            events_tx,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ApiTransfer> {
        self.events_tx.subscribe()
    }

    /// Registers a new outbound transfer or returns the existing one if `request_id` was
    /// already seen (`idempotent_replay = true`).
    pub fn register_outbound(
        &self,
        request_id: Option<String>,
        device_id: String,
        device_name: String,
        protocol: String,
        files: Vec<String>,
        total_bytes: u64,
    ) -> (ApiTransfer, bool) {
        let mut req_map = self.request_ids.lock().unwrap();
        let mut tmap = self.transfers.lock().unwrap();

        if let Some(ref rid) = request_id {
            if let Some(existing_tid) = req_map.get(rid) {
                if let Some(existing) = tmap.get(existing_tid) {
                    return (existing.clone(), true);
                }
            }
        }

        let transfer_id = format!("tr_{}", random_hex(8));
        if let Some(ref rid) = request_id {
            req_map.insert(rid.clone(), transfer_id.clone());
        }

        let tr = ApiTransfer {
            transfer_id: transfer_id.clone(),
            request_id,
            direction: "outbound".into(),
            protocol,
            device_id: Some(device_id),
            device_name,
            state: "connecting".into(),
            pin_code: None,
            files,
            saved_paths: Vec::new(),
            bytes_transferred: 0,
            total_bytes,
            progress: 0.0,
            error: None,
            updated_at_ms: now_ms(),
        };

        tmap.insert(transfer_id, tr.clone());
        let _ = self.events_tx.send(tr.clone());
        (tr, false)
    }

    /// Marks an existing transfer as failed with an explicit error reason.
    pub fn mark_failed(&self, transfer_id: &str, reason: impl Into<String>) {
        let mut tmap = self.transfers.lock().unwrap();
        if let Some(tr) = tmap.get_mut(transfer_id) {
            if !tr.is_terminal() {
                tr.state = "failed".into();
                tr.error = Some(reason.into());
                tr.updated_at_ms = now_ms();
                let _ = self.events_tx.send(tr.clone());
            }
        }
    }

    /// Ingests an internal `ChannelMessage` from Quick Share or AirDrop and broadcasts
    /// the normalized [`ApiTransfer`] update to all `/api/v1/events` subscribers.
    pub fn ingest_channel_message(&self, cm: &ChannelMessage) {
        let Message::Client(client) = &cm.msg else {
            return;
        };
        let Some(raw_state) = &client.state else {
            return;
        };

        let normalized = map_transfer_state(raw_state);
        let meta = client.metadata.as_ref();
        let direction = match client.kind {
            TransferKind::Inbound => "inbound",
            TransferKind::Outbound => "outbound",
        };
        let protocol = if cm.id.starts_with("airdrop-") {
            "airdrop"
        } else {
            "quickshare"
        };

        let mut tmap = self.transfers.lock().unwrap();
        let entry = tmap.entry(cm.id.clone()).or_insert_with(|| ApiTransfer {
            transfer_id: cm.id.clone(),
            request_id: None,
            direction: direction.into(),
            protocol: protocol.into(),
            device_id: None,
            device_name: meta
                .and_then(|m| m.source.as_ref().map(|s| s.name.clone()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Unknown device".into()),
            state: normalized.into(),
            pin_code: None,
            files: Vec::new(),
            saved_paths: Vec::new(),
            bytes_transferred: 0,
            total_bytes: 0,
            progress: 0.0,
            error: None,
            updated_at_ms: now_ms(),
        });

        // Do not let a trailing Disconnected overwrite an already completed/declined/cancelled transfer
        if normalized == "failed" && entry.is_terminal() {
            return;
        }

        entry.state = normalized.into();
        entry.updated_at_ms = now_ms();

        if let Some(m) = meta {
            if let Some(src) = &m.source {
                if !src.name.is_empty() {
                    entry.device_name = src.name.clone();
                }
            }
            if let Some(pin) = &m.pin_code {
                if !pin.is_empty() {
                    entry.pin_code = Some(pin.clone());
                }
            }
            if let Some(payload) = &m.payload {
                match payload {
                    TransferPayload::Files(list) if !list.is_empty() => {
                        entry.files = list.clone();
                    }
                    TransferPayload::Text(txt) if !txt.is_empty() => {
                        entry.files = vec![txt.clone()];
                    }
                    _ => {}
                }
            }
            if let Some(saved) = &m.saved_paths {
                if !saved.is_empty() {
                    entry.saved_paths = saved.clone();
                }
            }
            if m.total_bytes > 0 {
                entry.total_bytes = m.total_bytes;
            }
            if m.ack_bytes > 0 || entry.state == "completed" {
                entry.bytes_transferred = if entry.state == "completed" && entry.total_bytes > 0 {
                    entry.total_bytes
                } else {
                    m.ack_bytes.max(entry.bytes_transferred)
                };
            }
        }

        if entry.state == "completed" {
            if entry.total_bytes > 0 {
                entry.bytes_transferred = entry.total_bytes;
            }
            entry.progress = 1.0;
        } else if entry.total_bytes > 0 {
            entry.progress =
                (entry.bytes_transferred as f64 / entry.total_bytes as f64).clamp(0.0, 1.0);
        }

        let _ = self.events_tx.send(entry.clone());
    }

    pub fn list(&self) -> Vec<ApiTransfer> {
        let tmap = self.transfers.lock().unwrap();
        let mut out: Vec<ApiTransfer> = tmap.values().cloned().collect();
        out.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));
        out
    }

    pub fn get(&self, transfer_id: &str) -> Option<ApiTransfer> {
        self.transfers.lock().unwrap().get(transfer_id).cloned()
    }
}

fn map_transfer_state(s: &TransferState) -> &'static str {
    match s {
        TransferState::Initial
        | TransferState::ReceivedConnectionRequest
        | TransferState::SentUkeyClientInit
        | TransferState::SentUkeyServerInit
        | TransferState::SentUkeyClientFinish
        | TransferState::ReceivedUkeyClientFinish
        | TransferState::SentConnectionResponse
        | TransferState::SentPairedKeyEncryption
        | TransferState::SentPairedKeyResult
        | TransferState::ReceivedPairedKeyResult
        | TransferState::SentIntroduction => "connecting",
        TransferState::WaitingForUserConsent => "waiting_for_consent",
        TransferState::ReceivingFiles | TransferState::SendingFiles => "in_progress",
        TransferState::Finished => "completed",
        TransferState::Rejected => "declined",
        TransferState::Cancelled => "cancelled",
        TransferState::Disconnected => "failed",
    }
}

/// Shared bundle holding all `/api/v1` registries & auth state.
#[derive(Clone)]
pub struct ApiV1State {
    pub auth: ApiAuth,
    pub devices: DeviceRegistry,
    pub discovery: DiscoverySessionManager,
    pub transfers: TransferRegistry,
}

impl ApiV1State {
    pub fn new(
        bridge_port: u16,
        msg_tx: broadcast::Sender<ChannelMessage>,
        disc_tx: broadcast::Sender<EndpointInfo>,
    ) -> Self {
        let state = Self {
            auth: ApiAuth::init(bridge_port),
            devices: DeviceRegistry::new(),
            discovery: DiscoverySessionManager::new(),
            transfers: TransferRegistry::new(),
        };

        // Continuously feed all discovery & transfer broadcasts into the v1 registries
        let dev_reg = state.devices.clone();
        let mut d_rx = disc_tx.subscribe();
        tokio::spawn(async move {
            loop {
                match d_rx.recv().await {
                    Ok(ei) => {
                        dev_reg.ingest(&ei);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let tr_reg = state.transfers.clone();
        let mut m_rx = msg_tx.subscribe();
        tokio::spawn(async move {
            loop {
                match m_rx.recv().await {
                    Ok(cm) => tr_reg.ingest_channel_message(&cm),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_lan_and_ble_for_same_device_and_keeps_same_names_distinct() {
        let reg = DeviceRegistry::new();

        // 1. Pixel 9 discovered over LAN
        let ev1 = reg
            .ingest(&EndpointInfo {
                id: "A1B2".into(),
                name: Some("Pixel 9".into()),
                ip: Some("192.168.1.50".into()),
                port: Some("42000".into()),
                rtype: Some(DeviceType::Phone),
                present: Some(true),
                ..Default::default()
            })
            .unwrap();
        let id_first = ev1.device.device_id.clone();
        assert_eq!(ev1.device.transports, vec!["lan"]);

        // 2. Same Pixel 9 also discovered over BLE -> merges into same device_id!
        let ev2 = reg
            .ingest(&EndpointInfo {
                id: "ble://Pixel 9".into(),
                name: Some("Pixel 9".into()),
                ble_addr: Some("AA:BB:CC:DD:EE:01".into()),
                rtype: Some(DeviceType::Phone),
                present: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(ev2.device.device_id, id_first);
        assert_eq!(ev2.device.transports, vec!["lan", "ble"]);
        assert_eq!(ev2.device.preferred_transport.as_deref(), Some("lan"));

        // 3. A SECOND device ALSO named "Pixel 9" on a different IP -> stays distinct!
        let ev3 = reg
            .ingest(&EndpointInfo {
                id: "C3D4".into(),
                name: Some("Pixel 9".into()),
                ip: Some("192.168.1.99".into()),
                port: Some("43000".into()),
                rtype: Some(DeviceType::Phone),
                present: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert_ne!(ev3.device.device_id, id_first);

        let list = reg.list_reachable();
        assert_eq!(list.len(), 2);
        // Both identically named devices get a disambiguator!
        assert!(list.iter().all(|d| d.disambiguator.is_some()));

        // 4. First device drops LAN -> still reachable via BLE!
        let ev_drop_lan = reg
            .ingest(&EndpointInfo {
                id: "A1B2".into(),
                present: Some(false),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(ev_drop_lan.event_type, "upsert");
        assert_eq!(ev_drop_lan.device.transports, vec!["ble"]);
        assert_eq!(
            reg.resolve_route(&id_first),
            Some(ResolvedSendRoute::QuickShareBle {
                peer_name: "Pixel 9".into()
            })
        );
    }

    #[test]
    fn discovery_sessions_are_reference_counted() {
        let mgr = DiscoverySessionManager::new();
        let (s1, start1, cnt1, _) = mgr.create_session(Some("app".into()), Some(60));
        assert!(start1);
        assert_eq!(cnt1, 1);

        let (s2, start2, cnt2, _) = mgr.create_session(Some("kde-widget".into()), Some(60));
        assert!(!start2);
        assert_eq!(cnt2, 2);

        // Closing the widget must NOT stop hardware discovery while the app session is alive!
        let (existed, stop_hw, cnt_after) = mgr.remove_session(&s2);
        assert!(existed);
        assert!(!stop_hw);
        assert_eq!(cnt_after, 1);

        // Closing the last session stops hardware discovery.
        let (existed2, stop_hw2, cnt_final) = mgr.remove_session(&s1);
        assert!(existed2);
        assert!(stop_hw2);
        assert_eq!(cnt_final, 0);
    }

    #[test]
    fn transfers_are_idempotent_on_request_id() {
        let reg = TransferRegistry::new();
        let (t1, replay1) = reg.register_outbound(
            Some("req-123".into()),
            "dev_abc".into(),
            "Pixel 9".into(),
            "quickshare".into(),
            vec!["doc.pdf".into()],
            1024,
        );
        assert!(!replay1);
        assert_eq!(t1.state, "connecting");

        let (t2, replay2) = reg.register_outbound(
            Some("req-123".into()),
            "dev_abc".into(),
            "Pixel 9".into(),
            "quickshare".into(),
            vec!["doc.pdf".into()],
            1024,
        );
        assert!(replay2);
        assert_eq!(t2.transfer_id, t1.transfer_id);
    }
}
