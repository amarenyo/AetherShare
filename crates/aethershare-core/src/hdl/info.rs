use std::fs::File;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::sharing_nearby::wifi_credentials_metadata::SecurityType;
use crate::utils::RemoteDeviceInfo;

use super::TextPayloadType;

#[derive(Debug)]
pub struct InternalFileInfo {
    pub payload_id: i64,
    pub file_url: PathBuf,
    pub bytes_transferred: i64,
    pub total_size: i64,
    pub file: Option<File>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransferPayload {
    Files(Vec<String>),
    Text(String),
    Url(String),
    Wifi {
        ssid: String,
        password: String,
        #[serde(skip)]
        security_type: SecurityType,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum TransferPayloadKind {
    #[default]
    Files,
    Text,
    Url,
    WiFi,
}

impl TransferPayload {
    pub fn text_type(&self) -> Option<TextPayloadType> {
        match self {
            TransferPayload::Text(_) => Some(TextPayloadType::Text),
            TransferPayload::Url(_) => Some(TextPayloadType::Url),
            TransferPayload::Wifi { .. } => Some(TextPayloadType::Wifi),
            TransferPayload::Files(_) => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferMetadata {
    pub id: String,
    pub source: Option<RemoteDeviceInfo>,
    pub pin_code: Option<String>,

    pub payload_kind: TransferPayloadKind,
    pub payload_preview: Option<String>,
    pub payload: Option<TransferPayload>,

    pub destination: Option<String>,
    pub saved_paths: Option<Vec<String>>,

    pub total_bytes: u64,
    pub ack_bytes: u64,
}
