//! Smart network interface filtering & diagnostics for AetherShare.
//!
//! Prevents virtual/container/hypervisor interfaces (such as VMware `vmnet1`/`vmnet8`,
//! Docker `docker0`/`br-*`, libvirt `virbr*`, VPN `tun*`/`wg*`/`tailscale*`) from
//! hijacking mDNS announcements or Wi-Fi LAN bandwidth upgrades.

use std::net::Ipv4Addr;

use get_if_addrs::{IfAddr, Interface, get_if_addrs};
use serde::{Deserialize, Serialize};

/// Known virtual / non-LAN interface prefixes that should never be advertised
/// to nearby phones/laptops as our local Wi-Fi/Ethernet endpoint.
const IGNORED_IFACE_PREFIXES: &[&str] = &[
    "lo",
    "vmnet",
    "vboxnet",
    "docker",
    "virbr",
    "veth",
    "br-",
    "tun",
    "tap",
    "tailscale",
    "wg",
    "zt",
    "cni",
    "flannel",
    "podman",
    "lxcbr",
    " incusbr",
    "lxdbr",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceDiagnostic {
    pub name: String,
    pub ip: String,
    pub netmask: Option<String>,
    pub is_ipv6: bool,
    pub allowed: bool,
    pub reason: String,
    pub priority: u8,
}

/// Checks whether an interface name belongs to a virtual, VPN, or loopback interface.
pub fn is_virtual_or_ignored_interface(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();

    // Optional explicit allowlist override: AETHERSHARE_ALLOW_IFACES="wlp0s20f3,enp0s31f6"
    if let Ok(allow) = std::env::var("AETHERSHARE_ALLOW_IFACES") {
        let allowed: Vec<&str> = allow.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        if !allowed.is_empty() {
            return !allowed.iter().any(|a| a.eq_ignore_ascii_case(&lower));
        }
    }

    // Optional explicit denylist addition: AETHERSHARE_IGNORE_IFACES="eth2,foo0"
    if let Ok(ignore) = std::env::var("AETHERSHARE_IGNORE_IFACES") {
        for item in ignore.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if lower.starts_with(&item.to_ascii_lowercase()) {
                return true;
            }
        }
    }

    IGNORED_IFACE_PREFIXES
        .iter()
        .any(|prefix| lower == *prefix || lower.starts_with(prefix))
}

/// Ranks physical interfaces so Wi-Fi (`wl*`, `wlan*`) comes first, followed by
/// Ethernet (`en*`, `eth*`), followed by any other non-virtual interface.
pub fn interface_priority(name: &str) -> u8 {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("wl") || lower.starts_with("wifi") {
        1
    } else if lower.starts_with("en") || lower.starts_with("eth") {
        2
    } else {
        3
    }
}

/// Returns all non-virtual, non-loopback, non-link-local IPv4 interfaces sorted
/// by physical interface priority (Wi-Fi first, then Ethernet).
pub fn filtered_ipv4_interfaces() -> Vec<(String, Ipv4Addr, Ipv4Addr)> {
    let Ok(addrs) = get_if_addrs() else {
        return Vec::new();
    };

    let mut list: Vec<(u8, String, Ipv4Addr, Ipv4Addr)> = Vec::new();
    for ia in addrs {
        if is_virtual_or_ignored_interface(&ia.name) {
            continue;
        }
        if let IfAddr::V4(v4) = ia.addr {
            if v4.ip.is_loopback() || v4.ip.is_link_local() || v4.ip.is_unspecified() {
                continue;
            }
            let prio = interface_priority(&ia.name);
            list.push((prio, ia.name, v4.ip, v4.netmask));
        }
    }

    list.sort_by_key(|(prio, _, ip, _)| (*prio, !ip.is_private()));
    list.into_iter()
        .map(|(_, name, ip, mask)| (name, ip, mask))
        .collect()
}

/// Returns all allowed LAN IPv4 addresses (excluding `vmnet*`, `docker*`, etc.).
pub fn filtered_ipv4_addrs() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    for (_, ip, _) in filtered_ipv4_interfaces() {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// Primary LAN IPv4 address for Bandwidth Upgrade (`WIFI_LAN`) and mDNS.
pub fn primary_lan_ipv4() -> Option<[u8; 4]> {
    filtered_ipv4_addrs().first().map(Ipv4Addr::octets)
}

/// True if `remote` is in the same subnet as any of our allowed physical LAN interfaces.
pub fn same_lan_subnet(remote: [u8; 4]) -> bool {
    let r = u32::from(Ipv4Addr::from(remote));
    for (_, ip, netmask) in filtered_ipv4_interfaces() {
        let l = u32::from(ip);
        let mask = u32::from(netmask);
        if mask != 0 && (l & mask) == (r & mask) {
            return true;
        }
    }
    false
}

/// True if `ip_address` is NOT one of our own interface IPs AND does NOT fall
/// into a local virtual interface subnet (like `vmnet1` `192.168.230.0/24` or
/// `vmnet8` `192.168.255.0/24`).
pub fn is_valid_peer_ipv4(ip_address: &Ipv4Addr) -> bool {
    if ip_address.is_loopback() || ip_address.is_unspecified() {
        return false;
    }
    if let Ok(if_addrs) = get_if_addrs() {
        let target = u32::from(*ip_address);
        for ia in if_addrs {
            if ia.ip() == *ip_address {
                return false;
            }
            // Also reject addresses belonging to a virtual interface's subnet!
            if is_virtual_or_ignored_interface(&ia.name) {
                if let IfAddr::V4(v4) = ia.addr {
                    let mask = u32::from(v4.netmask);
                    let net = u32::from(v4.ip) & mask;
                    if mask != 0 && (target & mask) == net {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Ensures an mDNS host name is ASCII-safe and ends with `.local.`.
pub fn normalize_mdns_hostname(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(".local");
    let sanitized: String = trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let label = sanitized.trim_matches('-');
    let final_label = if label.is_empty() {
        "AetherShare"
    } else {
        label
    };
    format!("{final_label}.local.")
}

/// Produces a complete diagnostic report of all system network interfaces.
pub fn diagnose_interfaces() -> Vec<InterfaceDiagnostic> {
    let Ok(addrs): Result<Vec<Interface>, _> = get_if_addrs() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for ia in addrs {
        let (ip, netmask, is_ipv6, is_loop_or_ll) = match &ia.addr {
            IfAddr::V4(v4) => (
                v4.ip.to_string(),
                Some(v4.netmask.to_string()),
                false,
                v4.ip.is_loopback() || v4.ip.is_link_local(),
            ),
            IfAddr::V6(v6) => (
                v6.ip.to_string(),
                Some(v6.netmask.to_string()),
                true,
                v6.ip.is_loopback(),
            ),
        };

        let ignored = is_virtual_or_ignored_interface(&ia.name);
        let is_de = crate::i18n::active_lang() == crate::i18n::Lang::De;
        let (allowed, reason) = if is_loop_or_ll {
            (false, "Loopback / Link-Local".to_string())
        } else if ignored {
            (
                false,
                if is_de {
                    format!("Gefiltert (virtuelles Interface '{}')", ia.name)
                } else {
                    format!("Filtered (virtual interface '{}')", ia.name)
                },
            )
        } else if is_ipv6 {
            (
                true,
                if is_de {
                    "Aktiv (IPv6 / AirDrop)".to_string()
                } else {
                    "Active (IPv6 / AirDrop)".to_string()
                },
            )
        } else {
            (
                true,
                if is_de {
                    "Aktiv (Primäres LAN für Quick Share & AirDrop)".to_string()
                } else {
                    "Active (Primary LAN for Quick Share & AirDrop)".to_string()
                },
            )
        };

        out.push(InterfaceDiagnostic {
            priority: interface_priority(&ia.name),
            name: ia.name,
            ip,
            netmask,
            is_ipv6,
            allowed,
            reason,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_vmware_and_container_interfaces() {
        assert!(is_virtual_or_ignored_interface("vmnet1"));
        assert!(is_virtual_or_ignored_interface("vmnet8"));
        assert!(is_virtual_or_ignored_interface("docker0"));
        assert!(is_virtual_or_ignored_interface("virbr0"));
        assert!(is_virtual_or_ignored_interface("tun0"));
        assert!(is_virtual_or_ignored_interface("tailscale0"));
        assert!(!is_virtual_or_ignored_interface("wlp0s20f3"));
        assert!(!is_virtual_or_ignored_interface("enp0s31f6"));
    }

    #[test]
    fn normalizes_mdns_hostname_with_local_suffix() {
        assert_eq!(
            normalize_mdns_hostname("FedoraByAmare"),
            "FedoraByAmare.local."
        );
        assert_eq!(
            normalize_mdns_hostname("FedoraByAmare.local"),
            "FedoraByAmare.local."
        );
        assert_eq!(
            normalize_mdns_hostname("FedoraByAmare.local."),
            "FedoraByAmare.local."
        );
    }
}
