//! AirDrop `_airdrop._tcp.local.` mDNS / DNS-SD Server & Discovery.
//!
//! Uses `aethershare_core::net_filter` to ensure virtual interfaces (`vmnet1`, `vmnet8`,
//! `docker0`, etc.) are never advertised while physical LAN / `awdl0` / Wi-Fi Aware interfaces are.

use std::collections::HashMap;
use std::net::Ipv4Addr;

use aethershare_core::net_filter::{
    filtered_ipv4_addrs, is_valid_peer_ipv4, normalize_mdns_hostname,
};
use aethershare_core::{DeviceType, EndpointInfo};
use log::{info, warn};
use mdns_sd::{AddrType, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub const AIRDROP_MDNS_TYPE: &str = "_airdrop._tcp.local.";
/// Standard AirDrop feature flags: SupportsURL(1) | SupportsDVZip(2) | SupportsPipelining(4) |
/// SupportsMixedTypes(8) | SupportsUnknown1(0x10) | SupportsUnknown2(0x20) |
/// SupportsDiscover(0x40) | SupportsAskIcon(0x80) | SupportsAssetBundle(0x100) | SupportsLivePhotos(0x200)
pub const AIRDROP_DEFAULT_FLAGS: &str = "1019";

pub struct AirDropMdnsServer {
    instance_id: String,
    device_name: String,
    port: u16,
    daemon: ServiceDaemon,
}

impl AirDropMdnsServer {
    pub fn new(instance_id: String, device_name: String, port: u16) -> Result<Self, anyhow::Error> {
        let daemon = ServiceDaemon::new()?;
        Ok(Self {
            instance_id,
            device_name,
            port,
            daemon,
        })
    }

    pub async fn run(&self, ctk: CancellationToken) -> Result<(), anyhow::Error> {
        let allowed_ips = filtered_ipv4_addrs();
        let host_fqdn = normalize_mdns_hostname(&self.device_name);
        let ip_list: String = allowed_ips
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");

        let properties = [
            ("flags", AIRDROP_DEFAULT_FLAGS.to_string()),
            ("cname", self.device_name.clone()),
        ];

        let si = if allowed_ips.is_empty() {
            ServiceInfo::new(
                AIRDROP_MDNS_TYPE,
                &self.instance_id,
                &host_fqdn,
                "",
                self.port,
                &properties[..],
            )?
            .enable_addr_auto(AddrType::V4)
        } else {
            ServiceInfo::new(
                AIRDROP_MDNS_TYPE,
                &self.instance_id,
                &host_fqdn,
                &ip_list,
                self.port,
                &properties[..],
            )?
        };

        info!(
            "AirDropMdnsServer: advertising {} on host={} ips=[{}] port={}",
            self.instance_id, host_fqdn, ip_list, self.port
        );
        self.daemon.register(si.clone())?;

        ctk.cancelled().await;
        if let Ok(rx) = self.daemon.unregister(si.get_fullname()) {
            let _ = rx.recv();
        }
        Ok(())
    }
}

pub struct AirDropMdnsDiscovery {
    daemon: ServiceDaemon,
    sender: broadcast::Sender<EndpointInfo>,
}

impl AirDropMdnsDiscovery {
    pub fn new(sender: broadcast::Sender<EndpointInfo>) -> Result<Self, anyhow::Error> {
        Ok(Self {
            daemon: ServiceDaemon::new()?,
            sender,
        })
    }

    pub async fn run(self, ctk: CancellationToken) -> Result<(), anyhow::Error> {
        info!("AirDropMdnsDiscovery: browsing {AIRDROP_MDNS_TYPE}");
        let receiver = self.daemon.browse(AIRDROP_MDNS_TYPE)?;
        let mut cache: HashMap<String, EndpointInfo> = HashMap::new();

        loop {
            tokio::select! {
                _ = ctk.cancelled() => break,
                r = receiver.recv_async() => {
                    match r {
                        Ok(ServiceEvent::ServiceResolved(info)) => {
                            let port = info.get_port();
                            let candidate_ips: Vec<Ipv4Addr> = info
                                .get_addresses_v4()
                                .into_iter()
                                .copied()
                                .filter(is_valid_peer_ipv4)
                                .collect();
                            let Some(&ip) = candidate_ips.first() else {
                                continue;
                            };

                            let display_name = info
                                .get_property_val_str("cname")
                                .map(str::to_owned)
                                .unwrap_or_else(|| {
                                    info.get_hostname()
                                        .trim_end_matches('.')
                                        .trim_end_matches(".local")
                                        .to_string()
                                });
                            let fullname = info.get_fullname().to_string();
                            let id = format!("airdrop://{ip}:{port}");
                            let ei = EndpointInfo {
                                fullname: fullname.clone(),
                                id,
                                name: Some(display_name),
                                ip: Some(ip.to_string()),
                                port: Some(port.to_string()),
                                rtype: Some(DeviceType::Phone),
                                present: Some(true),
                                ble_addr: None,
                                ble_psm: None,
                                protocol: Some("airdrop".to_string()),
                            };
                            info!("AirDropMdnsDiscovery: found AirDrop endpoint {:?}", ei);
                            cache.insert(fullname, ei.clone());
                            let _ = self.sender.send(ei);
                        }
                        Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                            if let Some(ei) = cache.remove(&fullname) {
                                let _ = self.sender.send(EndpointInfo {
                                    id: ei.id,
                                    present: Some(false),
                                    ..Default::default()
                                });
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!("AirDropMdnsDiscovery: channel closed ({e}), stopping");
                            break;
                        }
                    }
                }
            }
        }
        let _ = self.daemon.stop_browse(AIRDROP_MDNS_TYPE);
        Ok(())
    }
}
