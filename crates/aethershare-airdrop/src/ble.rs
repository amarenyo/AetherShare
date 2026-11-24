//! Apple Continuity BLE AirDrop Beacon (`0x004C`, Type `0x05`).
//!
//! When an iPhone, Mac, or AirDrop-compatible Pixel opens the share sheet, it emits
//! a BLE manufacturer-specific advertisement with Company ID `0x004C` (Apple Inc.):
//! `[0x05, 0x12, 0x00*8, 0x01, hash1(2B), hash2(2B), hash3(2B), hash4(2B), 0x00]`

use sha2::{Digest, Sha256};

pub const APPLE_COMPANY_ID: u16 = 0x004C;
pub const CONTINUITY_TYPE_AIRDROP: u8 = 0x05;
pub const CONTINUITY_AIRDROP_LEN: u8 = 0x12; // 18 bytes

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirDropBleBeacon {
    pub version: u8,
    pub hashes: [[u8; 2]; 4],
}

impl AirDropBleBeacon {
    /// Creates an AirDrop Continuity beacon from up to 4 identity strings
    /// (email/phone/hostname), hashing each with SHA-256 and taking the first 2 bytes.
    pub fn from_identities(identities: &[&str]) -> Self {
        let mut hashes = [[0u8; 2]; 4];
        for (idx, id) in identities.iter().take(4).enumerate() {
            let digest = Sha256::digest(id.trim().to_ascii_lowercase().as_bytes());
            hashes[idx] = [digest[0], digest[1]];
        }
        Self {
            version: 0x01,
            hashes,
        }
    }

    /// Serializes into the 20-byte Apple Continuity manufacturer payload
    /// (excluding the 2-byte `0x004C` company ID prefix).
    pub fn to_manufacturer_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(20);
        buf.push(CONTINUITY_TYPE_AIRDROP);
        buf.push(CONTINUITY_AIRDROP_LEN);
        buf.extend_from_slice(&[0u8; 8]); // 8 bytes of zeros
        buf.push(self.version);
        for h in &self.hashes {
            buf.extend_from_slice(h);
        }
        buf.push(0x00);
        buf
    }

    /// Parses an Apple (`0x004C`) manufacturer data payload if it contains an
    /// AirDrop (`0x05`) Continuity TLV structure.
    pub fn parse_apple_manufacturer_data(data: &[u8]) -> Option<Self> {
        let mut offset = 0;
        while offset + 2 <= data.len() {
            let tlv_type = data[offset];
            let tlv_len = data[offset + 1] as usize;
            offset += 2;
            if offset + tlv_len > data.len() {
                break;
            }
            if tlv_type == CONTINUITY_TYPE_AIRDROP && tlv_len >= 17 {
                let body = &data[offset..offset + tlv_len];
                let version = body[8];
                let mut hashes = [[0u8; 2]; 4];
                for i in 0..4 {
                    hashes[i] = [body[9 + i * 2], body[10 + i * 2]];
                }
                return Some(Self { version, hashes });
            }
            offset += tlv_len;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_airdrop_ble_beacon() {
        let beacon = AirDropBleBeacon::from_identities(&["user@linux.local"]);
        let raw = beacon.to_manufacturer_bytes();
        assert_eq!(raw.len(), 20);
        assert_eq!(raw[0], 0x05);
        assert_eq!(raw[1], 0x12);
        let parsed = AirDropBleBeacon::parse_apple_manufacturer_data(&raw).unwrap();
        assert_eq!(parsed, beacon);
    }
}
