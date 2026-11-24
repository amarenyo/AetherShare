use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::DeviceType;
use crate::net_filter::is_valid_peer_ipv4;
use crate::utils::parse_mdns_endpoint_info;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const LIVENESS_INTERVAL: Duration = Duration::from_secs(5);
const LIVENESS_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EndpointInfo {
    pub fullname: String,
    pub id: String,
    pub name: Option<String>,
    pub ip: Option<String>,
    pub port: Option<String>,
    pub rtype: Option<DeviceType>,
    pub present: Option<bool>,
    pub ble_addr: Option<String>,
    pub ble_psm: Option<u16>,
    /// Protocol identifier: "quickshare" or "airdrop"
    #[serde(default)]
    pub protocol: Option<String>,
}

pub struct MDnsDiscovery {
    daemon: ServiceDaemon,
    sender: broadcast::Sender<EndpointInfo>,
}

impl MDnsDiscovery {
    pub fn new(sender: broadcast::Sender<EndpointInfo>) -> Result<Self, anyhow::Error> {
        let daemon = ServiceDaemon::new()?;
        Ok(Self { daemon, sender })
    }

    pub async fn run(self, ctk: CancellationToken) -> Result<(), anyhow::Error> {
        info!("MDnsDiscovery: service starting");

        let service_type = "_FC9F5ED42C8A._tcp.local.";
        let receiver = self.daemon.browse(service_type)?;
        let cache: Arc<Mutex<HashMap<String, EndpointInfo>>> = Arc::new(Mutex::new(HashMap::new()));
        let mut liveness_tick = tokio::time::interval(LIVENESS_INTERVAL);
        liveness_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = ctk.cancelled() => {
                    info!("MDnsDiscovery: tracker cancelled, breaking");
                    break;
                }
                _ = liveness_tick.tick() => {
                    // Re-verify that cached WLAN peers still have an open Quick Share TCP port.
                    // Phones often close Quick Share without sending an mDNS goodbye packet.
                    let snapshot: Vec<(String, EndpointInfo)> = {
                        cache.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                    };
                    for (fullname, ei) in snapshot {
                        let cache = cache.clone();
                        let sender = self.sender.clone();
                        let id = ei.id.clone();
                        tokio::spawn(async move {
                            let alive = matches!(
                                tokio::time::timeout(LIVENESS_TIMEOUT, TcpStream::connect(&id)).await,
                                Ok(Ok(_))
                            );
                            if !alive {
                                if cache.lock().unwrap().remove(&fullname).is_some() {
                                    info!("MDnsDiscovery: peer {id} no longer reachable, removing");
                                    let _ = sender.send(EndpointInfo {
                                        id,
                                        present: Some(false),
                                        ..Default::default()
                                    });
                                }
                            }
                        });
                    }
                }
                r = receiver.recv_async() => {
                    match r {
                        Ok(event) => {
                            match event {
                                ServiceEvent::ServiceResolved(info) => {
                                    let port = info.get_port();

                                    // Collect ALL valid non-self, non-virtual IPv4 addresses
                                    let candidate_ips: Vec<Ipv4Addr> = info
                                        .get_addresses_v4()
                                        .into_iter()
                                        .copied()
                                        .filter(is_valid_peer_ipv4)
                                        .collect();

                                    if candidate_ips.is_empty() {
                                        continue;
                                    }

                                    let Some(n) = info.get_property("n") else {
                                        continue;
                                    };

                                    let Ok((dt, dn)) = parse_mdns_endpoint_info(n.val_str()) else {
                                        continue;
                                    };

                                    // Ignore unnamed/hidden background advertisements
                                    if dn.trim().is_empty() {
                                        continue;
                                    }

                                    let fullname = info.get_fullname().to_string();
                                    let sender = self.sender.clone();
                                    let cache = cache.clone();

                                    // Probe candidate IPs asynchronously with a 2-second timeout
                                    // so an unreachable IP never blocks the mDNS event loop.
                                    tokio::spawn(async move {
                                        for ip in candidate_ips {
                                            let ip_port = format!("{ip}:{port}");
                                            if let Ok(Ok(_)) = tokio::time::timeout(
                                                PROBE_TIMEOUT,
                                                TcpStream::connect(&ip_port),
                                            )
                                            .await
                                            {
                                                let ei = EndpointInfo {
                                                    fullname: fullname.clone(),
                                                    id: ip_port,
                                                    name: Some(dn),
                                                    ip: Some(ip.to_string()),
                                                    port: Some(port.to_string()),
                                                    rtype: Some(dt),
                                                    present: Some(true),
                                                    ble_addr: None,
                                                    ble_psm: None,
                                                    protocol: Some("quickshare".to_string()),
                                                };
                                                info!("ServiceResolved: Resolved Quick Share device: {:?}", ei);
                                                cache.lock().unwrap().insert(fullname, ei.clone());
                                                let _ = sender.send(ei);
                                                break;
                                            }
                                        }
                                    });
                                }
                                ServiceEvent::ServiceRemoved(_, fullname) => {
                                    trace!("ServiceRemoved: checking if should remove {}", fullname);
                                    let removed = cache.lock().unwrap().remove(&fullname);

                                    if let Some(ei) = removed {
                                        info!("ServiceRemoved: Remove previous service: {}", fullname);
                                        let _ = self.sender.send(EndpointInfo {
                                            id: ei.id,
                                            present: Some(false),
                                            ..Default::default()
                                        });
                                    }
                                }
                                ServiceEvent::SearchStarted(_) | ServiceEvent::SearchStopped(_) => {}
                                _ => {}
                            }
                        },
                        Err(err) => {
                            // CRITICAL FIX: break out of loop when channel closes instead of
                            // spinning at 100% CPU and writing a 291 GB log file!
                            warn!("MDnsDiscovery: channel closed ({}), stopping discovery loop", err);
                            break;
                        }
                    }
                }
            }
        }

        let _ = self.daemon.stop_browse(service_type);
        Ok(())
    }
}
