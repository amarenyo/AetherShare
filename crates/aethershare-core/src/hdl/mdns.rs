use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mdns_sd::{AddrType, ServiceDaemon, ServiceInfo};
use tokio::sync::broadcast::Receiver;
use tokio::sync::watch;
use tokio::time::{Instant, interval_at};
use tokio_util::sync::CancellationToken;

use crate::DEVICE_NAME;
use crate::net_filter::{filtered_ipv4_addrs, normalize_mdns_hostname};
use crate::utils::{DeviceType, gen_mdns_endpoint_info, gen_mdns_name};

const INNER_NAME: &str = "MDnsServer";
const TICK_INTERVAL: Duration = Duration::from_secs(60);
const NET_CHECK_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Visibility {
    Visible = 0,
    Invisible = 1,
    Temporarily = 2,
}

#[allow(dead_code)]
impl Visibility {
    pub fn from_raw_value(value: u64) -> Self {
        match value {
            0 => Visibility::Visible,
            1 => Visibility::Invisible,
            2 => Visibility::Temporarily,
            _ => Visibility::Visible,
        }
    }
}

pub struct MDnsServer {
    endpoint_id: [u8; 4],
    service_port: u16,
    daemon: ServiceDaemon,
    service_info: ServiceInfo,
    active_ips: Vec<Ipv4Addr>,
    ble_receiver: Receiver<()>,
    visibility_sender: Arc<Mutex<watch::Sender<Visibility>>>,
    visibility_receiver: watch::Receiver<Visibility>,
}

impl MDnsServer {
    pub fn new(
        endpoint_id: [u8; 4],
        service_port: u16,
        ble_receiver: Receiver<()>,
        visibility_sender: Arc<Mutex<watch::Sender<Visibility>>>,
        visibility_receiver: watch::Receiver<Visibility>,
    ) -> Result<Self, anyhow::Error> {
        let active_ips = filtered_ipv4_addrs();
        let service_info =
            Self::build_service(endpoint_id, service_port, DeviceType::Laptop, &active_ips)?;

        Ok(Self {
            endpoint_id,
            service_port,
            daemon: ServiceDaemon::new()?,
            service_info,
            active_ips,
            ble_receiver,
            visibility_sender,
            visibility_receiver,
        })
    }

    pub async fn run(&mut self, ctk: CancellationToken) -> Result<(), anyhow::Error> {
        info!("{INNER_NAME}: service starting");
        let monitor = self.daemon.monitor()?;
        let mut visibility = *self.visibility_receiver.borrow();
        let mut interval = interval_at(Instant::now() + TICK_INTERVAL, TICK_INTERVAL);
        let mut net_check = interval_at(Instant::now() + NET_CHECK_INTERVAL, NET_CHECK_INTERVAL);

        // Register immediately if starting Visible or Temporarily
        if visibility != Visibility::Invisible {
            let _ = self.daemon.register(self.service_info.clone());
        }

        loop {
            tokio::select! {
                _ = ctk.cancelled() => {
                    info!("{INNER_NAME}: tracker cancelled, breaking");
                    break;
                }
                r = monitor.recv_async() => {
                    match r {
                        Ok(_) => continue,
                        Err(err) => {
                            warn!("{INNER_NAME}: daemon monitor closed ({err}), recreating ServiceDaemon");
                            if let Ok(new_daemon) = ServiceDaemon::new() {
                                self.daemon = new_daemon;
                                if visibility != Visibility::Invisible {
                                    let _ = self.daemon.register(self.service_info.clone());
                                }
                            } else {
                                return Err(err.into());
                            }
                        }
                    }
                },
                _ = self.visibility_receiver.changed() => {
                    visibility = *self.visibility_receiver.borrow_and_update();

                    debug!("{INNER_NAME}: visibility changed: {visibility:?}");
                    if visibility == Visibility::Visible {
                        self.daemon.register(self.service_info.clone())?;
                    } else if visibility == Visibility::Invisible {
                        let receiver = self.daemon.unregister(self.service_info.get_fullname())?;
                        let _ = receiver.recv();
                    } else if visibility == Visibility::Temporarily {
                        self.daemon.register(self.service_info.clone())?;
                        interval.reset();
                    }
                }
                _ = self.ble_receiver.recv() => {
                    if visibility == Visibility::Invisible {
                        continue;
                    }

                    debug!("{INNER_NAME}: ble_receiver: got event, refreshing mDNS announcement");
                    if visibility == Visibility::Visible || visibility == Visibility::Temporarily {
                        let _ = self.daemon.register_resend(self.service_info.get_fullname());
                    } else {
                        let _ = self.daemon.register(self.service_info.clone());
                    }
                },
                _ = net_check.tick() => {
                    // Watchdog for Wi-Fi IP changes or suspend/resume recovery
                    let current_ips = filtered_ipv4_addrs();
                    if current_ips != self.active_ips && !current_ips.is_empty() {
                        info!(
                            "{INNER_NAME}: LAN IPv4 addresses changed ({:?} -> {:?}), updating mDNS service",
                            self.active_ips, current_ips
                        );
                        self.active_ips = current_ips;
                        if let Ok(new_si) = Self::build_service(
                            self.endpoint_id,
                            self.service_port,
                            DeviceType::Laptop,
                            &self.active_ips,
                        ) {
                            self.service_info = new_si;
                            if visibility != Visibility::Invisible {
                                let _ = self.daemon.register(self.service_info.clone());
                            }
                        }
                    } else if visibility == Visibility::Visible || visibility == Visibility::Temporarily {
                        let _ = self.daemon.register_resend(self.service_info.get_fullname());
                    }
                }
                _ = interval.tick() => {
                    if visibility != Visibility::Temporarily {
                        continue;
                    }

                    let receiver = self.daemon.unregister(self.service_info.get_fullname())?;
                    let _ = receiver.recv();
                    let _ = self.visibility_sender.lock().unwrap().send(Visibility::Invisible);
                }
            }
        }

        let receiver = self.daemon.unregister(self.service_info.get_fullname())?;
        if let Ok(event) = receiver.recv() {
            info!("MDnsServer: service unregistered: {:?}", event);
        }

        Ok(())
    }

    fn build_service(
        endpoint_id: [u8; 4],
        service_port: u16,
        device_type: DeviceType,
        allowed_ips: &[Ipv4Addr],
    ) -> Result<ServiceInfo, anyhow::Error> {
        let name = gen_mdns_name(endpoint_id);
        let device_name = DEVICE_NAME.read().unwrap().clone();
        let host_fqdn = normalize_mdns_hostname(&device_name);

        let ip_list: String = allowed_ips
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");

        info!(
            "Broadcasting mDNS Quick Share service: device_name={device_name}, host={host_fqdn}, ips=[{ip_list}], port={service_port}"
        );
        let endpoint_info = gen_mdns_endpoint_info(device_type as u8, &device_name);
        let properties = [("n", endpoint_info)];

        let si = if allowed_ips.is_empty() {
            ServiceInfo::new(
                "_FC9F5ED42C8A._tcp.local.",
                &name,
                &host_fqdn,
                "",
                service_port,
                &properties[..],
            )?
            .enable_addr_auto(AddrType::V4)
        } else {
            // Explicitly advertise ONLY non-virtual LAN IPs (excludes vmnet1/vmnet8/docker0!)
            ServiceInfo::new(
                "_FC9F5ED42C8A._tcp.local.",
                &name,
                &host_fqdn,
                &ip_list,
                service_port,
                &properties[..],
            )?
        };

        Ok(si)
    }
}
