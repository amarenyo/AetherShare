//! Apple AirDrop Binary/XML Plist models for `/Discover`, `/Ask`, and `/Upload`.

use std::io::Cursor;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoverRequest {
    #[serde(rename = "SenderRecordData")]
    pub sender_record_data: Option<plist::Data>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverResponse {
    #[serde(rename = "ReceiverComputerName")]
    pub receiver_computer_name: String,
    #[serde(rename = "ReceiverModelName")]
    pub receiver_model_name: String,
    #[serde(rename = "ReceiverMediaCapabilities")]
    pub receiver_media_capabilities: plist::Data,
}

impl DiscoverResponse {
    pub fn new(computer_name: &str) -> Self {
        let caps_json = br#"{"Version":1,"Vendor":{"com.apple":{"LivePhotoFormatVersion":"1"}}}"#;
        Self {
            receiver_computer_name: computer_name.to_string(),
            receiver_model_name: "MacBookPro18,1".to_string(),
            receiver_media_capabilities: plist::Data::new(caps_json.to_vec()),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AirDropFileEntry {
    #[serde(rename = "FileName")]
    pub file_name: String,
    #[serde(rename = "FileType")]
    pub file_type: Option<String>,
    #[serde(rename = "FileBomPath")]
    pub file_bom_path: Option<String>,
    #[serde(rename = "FileIsDirectory")]
    pub file_is_directory: Option<bool>,
    #[serde(rename = "ConvertMediaFormats")]
    pub convert_media_formats: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AskRequest {
    #[serde(rename = "SenderComputerName")]
    pub sender_computer_name: Option<String>,
    #[serde(rename = "SenderModelName")]
    pub sender_model_name: Option<String>,
    #[serde(rename = "SenderID")]
    pub sender_id: Option<String>,
    #[serde(rename = "BundleID")]
    pub bundle_id: Option<String>,
    #[serde(rename = "ConvertMediaFormats")]
    pub convert_media_formats: Option<bool>,
    #[serde(rename = "Files", default)]
    pub files: Vec<AirDropFileEntry>,
    #[serde(rename = "Items", default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskResponse {
    #[serde(rename = "ReceiverModelName")]
    pub receiver_model_name: String,
    #[serde(rename = "ReceiverComputerName")]
    pub receiver_computer_name: String,
}

impl AskResponse {
    pub fn new(computer_name: &str) -> Self {
        Self {
            receiver_model_name: "MacBookPro18,1".to_string(),
            receiver_computer_name: computer_name.to_string(),
        }
    }
}

/// Decodes either a Binary Plist (`bplist00`) or XML Plist (`<?xml ...`) payload.
pub fn decode_plist<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, anyhow::Error> {
    if bytes.is_empty() {
        return Err(anyhow!("Empty plist body"));
    }
    plist::from_bytes(bytes).map_err(|e| anyhow!("Failed to decode plist: {e}"))
}

/// Encodes a struct into Apple Binary Plist (`application/octet-stream`).
pub fn encode_binary_plist<T: Serialize>(val: &T) -> Result<Vec<u8>, anyhow::Error> {
    let mut cursor = Cursor::new(Vec::new());
    plist::to_writer_binary(&mut cursor, val)?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_plist_discover_and_ask_roundtrip() {
        let resp = DiscoverResponse::new("FedoraByAmare");
        let bytes = encode_binary_plist(&resp).unwrap();
        assert!(bytes.starts_with(b"bplist00"));
        let decoded: DiscoverResponse = decode_plist(&bytes).unwrap();
        assert_eq!(decoded.receiver_computer_name, "FedoraByAmare");

        let ask = AskRequest {
            sender_computer_name: Some("Pixel 9 Pro".to_string()),
            sender_model_name: Some("Pixel 9 Pro".to_string()),
            sender_id: Some("a1b2c3d4e5f6".to_string()),
            files: vec![AirDropFileEntry {
                file_name: "photo.jpg".to_string(),
                file_type: Some("public.jpeg".to_string()),
                file_bom_path: Some("./photo.jpg".to_string()),
                file_is_directory: Some(false),
                convert_media_formats: Some(false),
            }],
            ..Default::default()
        };
        let ask_bytes = encode_binary_plist(&ask).unwrap();
        let ask_dec: AskRequest = decode_plist(&ask_bytes).unwrap();
        assert_eq!(ask_dec.sender_computer_name.as_deref(), Some("Pixel 9 Pro"));
        assert_eq!(ask_dec.files.len(), 1);
        assert_eq!(ask_dec.files[0].file_name, "photo.jpg");
    }
}
