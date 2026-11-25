//! Apple AirDrop & Quick Share Cross-Platform AirDrop Interoperability Engine.
//!
//! Implements:
//! - Apple Continuity BLE AirDrop Advertisement & Scanner (Company ID `0x004C`, Type `0x05`)
//! - `_airdrop._tcp.local.` mDNS / DNS-SD advertisement & browser (with virtual interface filtering)
//! - AirDrop HTTP/1.1 over TLS (and plain fallback) `/Discover`, `/Ask`, and `/Upload` server & client
//! - Binary & XML Plist encoding/decoding
//! - Pure-Rust POSIX.1 ODC (`070707`) CPIO + Gzip archive packing and safe extraction

pub mod archive;
pub mod awdl;
pub mod ble;
pub mod bridge_v1;
pub mod client;
pub mod mdns;
pub mod plist_codec;
pub mod server;

pub use awdl::{AirDropManager, AirDropRadioStatus};
pub use client::AirDropClient;
pub use mdns::{AirDropMdnsDiscovery, AirDropMdnsServer};
pub use server::AirDropServer;
