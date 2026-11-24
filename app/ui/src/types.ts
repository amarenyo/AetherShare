export type VisibilityMode = 'Visible' | 'Temporarily' | 'Invisible';

export interface InterfaceDiagnostic {
  name: string;
  ip: string;
  is_ipv6: boolean;
  allowed: boolean;
  reason: string;
}

export interface StatusResponse {
  device_name: string;
  visibility: VisibilityMode;
  qs_port: number | null;
  airdrop_port: number;
  active_lan_ips: string[];
  interfaces: InterfaceDiagnostic[];
  download_dir: string;
  auto_accept: boolean;
  l2cap_psm?: number | null;
  system_accent?: string | null;
  system_color_scheme?: 'dark' | 'light' | null;
  system_lang?: 'de' | 'en' | null;
  version?: string;
}

export interface EndpointInfo {
  id: string;
  name?: string;
  ip?: string;
  port?: string;
  rtype?: string; // 'Phone' | 'Tablet' | 'Laptop' | 'Unknown'
  ble_addr?: string | null;
  ble_psm?: number | null;
  protocol?: string | null;
  present?: boolean | null;
}

export interface TransferMetadata {
  id: string;
  source?: { name: string; device_type: string };
  pin_code?: string;
  payload_kind?: string;
  payload?: { Files?: string[]; Text?: string } | Record<string, unknown>;
  destination?: string;
  saved_paths?: string[];
  total_bytes?: number;
  ack_bytes?: number;
}

export interface ChannelMessage {
  id: string;
  msg: {
    Client?: {
      state?: string;
      kind?: 'Inbound' | 'Outbound';
      metadata?: TransferMetadata;
    };
  };
}

export interface PickedFile {
  path: string;
  name: string;
  size: number;
}

export interface TransferRecord {
  id: string;
  kind: 'Inbound' | 'Outbound';
  peerName: string;
  state: string;
  pinCode?: string;
  files: string[];
  savedPaths: string[];
  destination?: string;
  totalBytes: number;
  ackBytes: number;
  updatedAt: number;
}
