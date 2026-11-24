use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use hkdf::Hkdf;
use num_bigint::{BigUint, ToBigInt};
use once_cell::sync::Lazy;
use p256::elliptic_curve::Generate;
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use rand::{Rng, RngExt};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DeviceType {
    #[default]
    Unknown = 0,
    Phone = 1,
    Tablet = 2,
    Laptop = 3,
}

impl DeviceType {
    pub fn from_raw_value(value: u8) -> Self {
        match value {
            1 => DeviceType::Phone,
            2 => DeviceType::Tablet,
            3 => DeviceType::Laptop,
            _ => DeviceType::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteDeviceInfo {
    pub name: String,
    pub device_type: DeviceType,
}

impl RemoteDeviceInfo {
    pub fn serialize(&self) -> Vec<u8> {
        let mut endpoint_info: Vec<u8> = vec![((self.device_type as u8) << 1) & 0b111];
        endpoint_info.extend((0..16).map(|_| rand::rng().random_range(0..=255)));

        let mut name_chars = self.name.as_bytes().to_vec();
        if name_chars.len() > 255 {
            name_chars.truncate(255);
        }
        endpoint_info.push(name_chars.len() as u8);
        endpoint_info.extend(name_chars);
        endpoint_info
    }
}

/// Shared 16-byte identity (2B salt + 14B metadata-key hash) used across both
/// mDNS and BLE advertisements so Android merges them into a single device card.
pub static ENDPOINT_IDENTITY: Lazy<[u8; 16]> = Lazy::new(|| rand::rng().random());

pub fn gen_mdns_name(endpoint_id: [u8; 4]) -> String {
    let mut name_b = Vec::with_capacity(10);
    name_b.push(0x23);
    name_b.extend_from_slice(&endpoint_id);
    name_b.extend_from_slice(&[0xFC, 0x9F, 0x5E]);
    name_b.extend_from_slice(&[0x00, 0x00]);
    URL_SAFE_NO_PAD.encode(&name_b)
}

pub fn gen_mdns_endpoint_info(device_type: u8, device_name: &str) -> String {
    let mut record = Vec::new();
    record.push((device_type & 0x7) << 1);
    record.extend_from_slice(ENDPOINT_IDENTITY.as_slice());

    let mut device_name_bytes = device_name.as_bytes().to_vec();
    device_name_bytes.truncate(255);
    record.push(device_name_bytes.len() as u8);
    record.extend_from_slice(&device_name_bytes);

    URL_SAFE_NO_PAD.encode(&record)
}

pub fn parse_mdns_endpoint_info(encoded_str: &str) -> Result<(DeviceType, String), anyhow::Error> {
    let decoded_bytes = URL_SAFE_NO_PAD.decode(encoded_str)?;
    if decoded_bytes.len() < 19 {
        return Err(anyhow!("Invalid data length"));
    }

    let device_type = (decoded_bytes[0] >> 1) & 0x7;
    let name_length = decoded_bytes[17] as usize;
    if 18 + name_length > decoded_bytes.len() {
        return Err(anyhow!("Invalid name length"));
    }

    let device_name_bytes = &decoded_bytes[18..18 + name_length];
    let device_name = String::from_utf8(device_name_bytes.to_vec())?;

    Ok((DeviceType::from_raw_value(device_type), device_name))
}

pub fn gen_ecdsa_keypair() -> (SecretKey, PublicKey) {
    let secret_key = SecretKey::generate();
    let public_key = secret_key.public_key();
    (secret_key, public_key)
}

fn p256_coordinate(c: &[u8]) -> Result<[u8; 32], anyhow::Error> {
    let significant = match c.iter().position(|&b| b != 0) {
        Some(i) => &c[i..],
        None => &[][..],
    };
    if significant.len() > 32 {
        return Err(anyhow!("P-256 coordinate too long ({} bytes)", c.len()));
    }
    let mut out = [0u8; 32];
    out[32 - significant.len()..].copy_from_slice(significant);
    Ok(out)
}

pub fn p256_public_key(x: &[u8], y: &[u8]) -> Result<PublicKey, anyhow::Error> {
    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04);
    sec1.extend_from_slice(&p256_coordinate(x)?);
    sec1.extend_from_slice(&p256_coordinate(y)?);
    PublicKey::from_sec1_bytes(&sec1).map_err(|_| anyhow!("Peer sent an invalid P-256 public key"))
}

pub fn public_key_xy(public_key: &PublicKey) -> Result<(Vec<u8>, Vec<u8>), anyhow::Error> {
    let pt = public_key.to_sec1_point(false);
    let x = pt.x().ok_or_else(|| anyhow!("Missing X coordinate"))?;
    let y = pt.y().ok_or_else(|| anyhow!("Missing Y coordinate"))?;
    Ok((
        encode_point(Bytes::from(x.to_vec()))?,
        encode_point(Bytes::from(y.to_vec()))?,
    ))
}

pub fn encode_point(unsigned: Bytes) -> Result<Vec<u8>, anyhow::Error> {
    let big_int = BigUint::from_bytes_be(&unsigned)
        .to_bigint()
        .ok_or_else(|| anyhow!("Failed to convert to bigint"))?;
    Ok(big_int.to_signed_bytes_be())
}

pub fn hkdf_extract_expand(
    salt: &[u8],
    input: &[u8],
    info: &[u8],
    output_len: usize,
) -> Result<Vec<u8>, anyhow::Error> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), input);
    let mut okm = vec![0u8; output_len];
    hkdf.expand(info, &mut okm)
        .map_err(|e| anyhow!("HKDF expand failed: {}", e))?;
    Ok(okm)
}

pub fn to_four_digit_string(bytes: &[u8]) -> String {
    let k_hash_modulo = 9973;
    let k_hash_base_multiplier = 31;

    let mut hash = 0;
    let mut multiplier = 1;
    for &byte in bytes {
        let byte = byte as i8 as i32;
        hash = (hash + byte * multiplier) % k_hash_modulo;
        multiplier = (multiplier * k_hash_base_multiplier) % k_hash_modulo;
    }

    format!("{:04}", hash.abs())
}

pub fn gen_random(size: usize) -> Vec<u8> {
    let mut data = vec![0; size];
    rand::rng().fill_bytes(&mut data);
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mdns_info_roundtrip() {
        let info = gen_mdns_endpoint_info(DeviceType::Laptop as u8, "AetherShareFedora");
        let (dt, name) = parse_mdns_endpoint_info(&info).unwrap();
        assert_eq!(dt, DeviceType::Laptop);
        assert_eq!(name, "AetherShareFedora");
    }

    #[test]
    fn test_p256_roundtrip() {
        let (_sk, pk) = gen_ecdsa_keypair();
        let (x, y) = public_key_xy(&pk).unwrap();
        let parsed = p256_public_key(&x, &y).unwrap();
        assert_eq!(pk, parsed);
    }

    #[test]
    fn test_p256_rejects_invalid_points() {
        assert!(p256_public_key(&[1u8; 32], &[2u8; 32]).is_err());
        assert!(p256_public_key(&[], &[]).is_err());
    }
}
