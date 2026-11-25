import type { AirDropRadioStatus, EndpointInfo, PickedFile, StatusResponse, VisibilityMode } from '../types';

declare global {
  interface Window {
    __AETHERSHARE_API_BASE__?: string;
    __TAURI_INTERNALS__?: unknown;
  }
}

export function apiBase(): string {
  if (window.__AETHERSHARE_API_BASE__) {
    return window.__AETHERSHARE_API_BASE__;
  }
  // When running inside Tauri webview (tauri:// or http://tauri.localhost), talk to local daemon bridge
  if (
    window.location.protocol === 'tauri:' ||
    window.location.hostname === 'tauri.localhost' ||
    window.location.protocol === 'file:'
  ) {
    return 'http://127.0.0.1:8787';
  }
  return '';
}

export function isTauriEnv(): boolean {
  return Boolean(window.__TAURI_INTERNALS__);
}

export async function fetchStatus(): Promise<StatusResponse> {
  const res = await fetch(`${apiBase()}/api/status`);
  if (!res.ok) throw new Error(`Status HTTP ${res.status}`);
  return res.json();
}

export async function startDiscovery(): Promise<void> {
  await fetch(`${apiBase()}/api/discovery/start`, { method: 'POST' });
}

export async function stopDiscovery(): Promise<void> {
  await fetch(`${apiBase()}/api/discovery/stop`, { method: 'POST' });
}

export async function setVisibility(visibility: VisibilityMode): Promise<void> {
  await fetch(`${apiBase()}/api/visibility`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ visibility }),
  });
}

export async function sendTransferAction(
  id: string,
  action: 'Accept' | 'Decline' | 'Cancel'
): Promise<void> {
  await fetch(`${apiBase()}/api/transfer/action`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ id, action }),
  });
}

export async function sendFilesToDevice(
  device: EndpointInfo,
  files: string[]
): Promise<void> {
  const isAirDrop = device.id.startsWith('airdrop://');
  const addr = device.ip && device.port ? `${device.ip}:${device.port}` : '';
  const res = await fetch(`${apiBase()}/api/send`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      id: device.id,
      name: device.name || 'Device',
      addr,
      ble: Boolean(device.ble_addr),
      protocol: isAirDrop ? 'airdrop' : 'quickshare',
      files,
    }),
  });
  if (!res.ok) throw new Error(`Send failed (${res.status})`);
}

export async function pickFilesNative(): Promise<PickedFile[]> {
  const res = await fetch(`${apiBase()}/api/pick-files`, { method: 'POST' });
  if (!res.ok) return [];
  const data = await res.json();
  return data.files || [];
}

export async function pickFolderNative(): Promise<string | null> {
  const res = await fetch(`${apiBase()}/api/pick-folder`, { method: 'POST' });
  if (!res.ok) return null;
  const data = await res.json();
  return data.path || null;
}

export async function openPathNative(path: string): Promise<void> {
  await fetch(`${apiBase()}/api/open-path`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path }),
  });
}

export async function updateSettings(patch: {
  auto_accept?: boolean;
  download_dir?: string;
  device_name?: string;
  language?: 'system' | 'de' | 'en';
}): Promise<void> {
  await fetch(`${apiBase()}/api/settings`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(patch),
  });
}

export async function setAirDropEnabled(
  enabled: boolean
): Promise<{ ok: boolean; error?: string; airdrop?: AirDropRadioStatus }> {
  const res = await fetch(`${apiBase()}/api/airdrop/${enabled ? 'enable' : 'disable'}`, {
    method: 'POST',
  });
  return res.json();
}

export function formatBytes(bytes: number): string {
  if (!bytes || bytes <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
  const val = bytes / Math.pow(1024, i);
  return `${val >= 10 || i === 0 ? val.toFixed(0) : val.toFixed(1)} ${units[i]}`;
}
