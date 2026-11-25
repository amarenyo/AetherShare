# AetherShare Local API v1 & Session D-Bus Reference

AetherShare exposes a versioned, token-authenticated local HTTP + Server-Sent Events (SSE) API (`/api/v1`) alongside a Session D-Bus activation service (`org.aethershare.Service`).

This interface is designed for external desktop integrations such as **KDE Plasma Widgets / Dynamic Islands**, scripts, and companion frontends.

---

## Table of Contents

1. [Discovery & Authentication](#1-discovery--authentication)
2. [Session D-Bus Auto-Activation](#2-session-d-bus-auto-activation)
3. [Error Format & Error Codes](#3-error-format--error-codes)
4. [Device Model: Merging, Stable IDs & Disambiguation](#4-device-model-merging-stable-ids--disambiguation)
5. [Discovery Sessions (Multi-Surface Reference Counting)](#5-discovery-sessions-multi-surface-reference-counting)
6. [Transfer State Machine & Idempotency](#6-transfer-state-machine--idempotency)
7. [HTTP & SSE Endpoint Reference](#7-http--sse-endpoint-reference)
   - [`GET /api/v1/health`](#get-apiv1health)
   - [`GET /api/v1/status`](#get-apiv1status)
   - [`GET /api/v1/devices`](#get-apiv1devices)
   - [`POST /api/v1/discovery/sessions`](#post-apiv1discoverysessions)
   - [`GET /api/v1/discovery/sessions`](#get-apiv1discoverysessions)
   - [`POST /api/v1/discovery/sessions/{id}/heartbeat`](#post-apiv1discoverysessionsidheartbeat)
   - [`DELETE /api/v1/discovery/sessions/{id}`](#delete-apiv1discoverysessionsid)
   - [`GET /api/v1/transfers`](#get-apiv1transfers)
   - [`POST /api/v1/transfers`](#post-apiv1transfers)
   - [`GET /api/v1/transfers/{id}`](#get-apiv1transfersid)
   - [`POST /api/v1/transfers/{id}/cancel` / `DELETE /api/v1/transfers/{id}`](#post-apiv1transfersidcancel--delete-apiv1transfersid)
   - [`POST /api/v1/transfers/{id}/accept`](#post-apiv1transfersidaccept)
   - [`POST /api/v1/transfers/{id}/decline`](#post-apiv1transfersiddecline)
   - [`POST /api/v1/visibility`](#post-apiv1visibility)
   - [`POST /api/v1/airdrop/enable`](#post-apiv1airdropenable)
   - [`POST /api/v1/airdrop/disable`](#post-apiv1airdropdisable)
   - [`GET /api/v1/events` (SSE)](#get-apiv1events-sse)
8. [Widget Integration Guide (KDE Plasma / QML / Shell)](#8-widget-integration-guide-kde-plasma--qml--shell)

---

## 1. Discovery & Authentication

The local API binds exclusively to loopback (`127.0.0.1`, default port `8787`).

On startup, AetherShare creates two private (`0600`) files in `$XDG_RUNTIME_DIR/aethershare/` (falling back to `$HOME/.local/state/aethershare/` if `XDG_RUNTIME_DIR` is unset):

| File | Permissions | Contents |
|---|---|---|
| `$XDG_RUNTIME_DIR/aethershare/api.token` | `0600` | 64-character hex local access token (reused across restarts in the same user session) |
| `$XDG_RUNTIME_DIR/aethershare/api.json` | `0600` | JSON discovery metadata (`url`, `api_base`, `token_file`, `version`, `pid`) |

Example `api.json`:

```json
{
  "url": "http://127.0.0.1:8787",
  "api_base": "http://127.0.0.1:8787/api/v1",
  "token_file": "/run/user/1000/aethershare/api.token",
  "version": "0.3.0",
  "pid": 42190
}
```

### Passing the Access Token

All `/api/v1/*` endpoints **except** `GET /api/v1/health` require the local access token via either:

1. **HTTP Header (recommended for REST calls):**
   ```http
   Authorization: Bearer <token>
   ```
2. **Query Parameter (for browser / QML `EventSource` SSE clients that cannot set custom headers):**
   ```text
   GET /api/v1/events?token=<token>
   ```

Token comparisons use constant-time equality. Missing or invalid tokens return `401 Unauthorized` with error code `UNAUTHORIZED`.

---

## 2. Session D-Bus Auto-Activation

AetherShare installs a Session D-Bus service activation file (`org.aethershare.Service.service`):

- System packages (`.rpm`, `.deb`, Arch `.pkg.tar.zst`): `/usr/share/dbus-1/services/org.aethershare.Service.service`
- User-local installs: `~/.local/share/dbus-1/services/org.aethershare.Service.service`

When a widget calls any method on `org.aethershare.Service`, D-Bus automatically launches `aethershare-desktop --background` (silently minimized to the system tray) if AetherShare is not already running.

### Bus Coordinates

- **Bus:** Session Bus (`--user`)
- **Service Name:** `org.aethershare.Service`
- **Object Path:** `/org/aethershare/Service`
- **Interface:** `org.aethershare.Service`

### Methods

| Method | Signature | Description |
|---|---|---|
| `Ping()` | `() -> (s)` | Returns `"pong"`. Lightweight way to auto-activate AetherShare on demand. |
| `GetApiInfo()` | `() -> (sss)` | Returns `(url, token_file, version)`, e.g. `("http://127.0.0.1:8787", "/run/user/1000/aethershare/api.token", "0.3.0")`. |
| `ShowWindow()` | `() -> (b)` | Shows, unminimizes, and focuses the main AetherShare desktop window. Returns `true` if the window exists. |
| `Quit()` | `() -> ()` | Cleanly disables AirDrop radio (if active) and exits AetherShare. |

### CLI Examples

```bash
# Auto-start AetherShare in the background (if not running) and fetch API coordinates:
busctl --user call org.aethershare.Service /org/aethershare/Service \
  org.aethershare.Service GetApiInfo

# Equivalent with gdbus:
gdbus call --session \
  --dest org.aethershare.Service \
  --object-path /org/aethershare/Service \
  --method org.aethershare.Service.GetApiInfo

# Bring the main desktop window to the foreground:
busctl --user call org.aethershare.Service /org/aethershare/Service \
  org.aethershare.Service ShowWindow
```

> **Single-Instance Behavior:** Launching `aethershare-desktop` normally while an instance was already D-Bus-activated in `--background` automatically calls `ShowWindow()` on the running instance and exits without port conflicts.

---

## 3. Error Format & Error Codes

Every error response under `/api/v1/*` uses a consistent JSON envelope:

```json
{
  "error": {
    "code": "DEVICE_UNREACHABLE",
    "message": "Device 'dev_a1b2c3d4e5f60718' is currently unreachable"
  }
}
```

### Standard Error Codes

| HTTP Status | `error.code` | Meaning |
|---|---|---|
| `400` | `INVALID_REQUEST` | Malformed JSON, empty `device_id`, or invalid field value (e.g. unknown visibility). |
| `401` | `UNAUTHORIZED` | Missing or invalid local access token. |
| `404` | `NOT_FOUND` | Unknown API route. |
| `404` | `DEVICE_NOT_FOUND` | `device_id` does not match any known device. |
| `404` | `SESSION_NOT_FOUND` | Discovery `session_id` does not exist or has expired. |
| `404` | `TRANSFER_NOT_FOUND` | `transfer_id` does not exist. |
| `409` | `DEVICE_UNREACHABLE` | Device was seen earlier, but currently has no reachable transport. |
| `409` | `AIRDROP_UNAVAILABLE` | AWDL radio daemon (`aethershare-awdld`) unavailable or activation failed. |
| `422` | `EMPTY_FILES` | `files` contained no existing readable files (directories are expanded recursively). |

---

## 4. Device Model: Merging, Stable IDs & Disambiguation

External widgets never need to handle raw mDNS addresses, IPv6 link-local zones, or BLE MAC addresses. The backend `DeviceRegistry` handles all merging and transport selection:

1. **Stable `device_id` (`dev_<16hex>`):**
   - Derived deterministically from the peer's stable identity (e.g. AirDrop IPv6 link-local address core, Quick Share endpoint ID / BLE address).
2. **Automatic LAN + BLE Merging:**
   - When the same Quick Share device is discovered over both LAN (`mdns`) and Bluetooth (`ble`), the backend merges both transports under **one** `device_id` with `transports: ["lan", "ble"]` and `preferred_transport: "lan"`.
   - If LAN disappears while BLE remains reachable, the device stays reachable (`preferred_transport: "ble"`) and emits an `event: device` (`upsert`) rather than vanishing.
3. **Same-Name Disambiguation:**
   - Two distinct physical devices with the exact same name (for example two `"Pixel 9"` phones on different IPs or endpoints) receive **different** `device_id`s and a non-null `disambiguator` string (e.g. `"LAN · .42"`, `"BLE · 1A:2B"`, `"AWDL · 3f4a"`) so the UI can distinguish them clearly.
   - When a name is unique among reachable devices, `disambiguator` is `null`.
4. **Automatic Transport Selection:**
   - When creating a transfer via `POST /api/v1/transfers`, the client only supplies `device_id`. AetherShare automatically chooses the best available route in priority order: **AWDL (AirDrop)** → **Quick Share LAN** → **Quick Share BLE (with automatic Wi-Fi Bandwidth Upgrade)**.

### `ApiDevice` Object Schema

```json
{
  "device_id": "dev_8f31c2a94b10e7d5",
  "name": "Pixel 9",
  "disambiguator": null,
  "device_type": "phone",
  "protocol": "quickshare",
  "transports": ["lan", "ble"],
  "preferred_transport": "lan",
  "reachable": true,
  "last_seen_ms": 1764081234567
}
```

- `device_type`: `"phone"`, `"tablet"`, `"laptop"`, or `"unknown"`
- `protocol`: `"quickshare"` or `"airdrop"`
- `transports`: array subset of `["lan", "ble", "awdl"]`
- `preferred_transport`: `"lan"`, `"ble"`, `"awdl"`, or `null` (when `reachable` is `false`)

---

## 5. Discovery Sessions (Multi-Surface Reference Counting)

Multiple UI surfaces (the main AetherShare desktop window, a KDE Plasma widget, a Dynamic Island popup, CLI scripts) can independently request device discovery without interfering with one another:

- Each surface creates its own session via `POST /api/v1/discovery/sessions` and receives a `session_id` (`ds_<24hex>`).
- The built-in desktop UI uses an internal named session (`"builtin-ui"`).
- Hardware scanning (mDNS + BLE + AirDrop probing) starts as soon as the active session count rises from `0` to `1` and **only stops when the last active session is deleted or expires**.
- Closing a KDE widget (`DELETE /api/v1/discovery/sessions/{id}`) will **never** interrupt an active scan in the main desktop app, and vice versa.
- Every session has a **TTL** (default `300s`, clamped to `5s..3600s`). A background reaper checks every 10 seconds and automatically stops hardware scanning if a client crashes without deleting its session. Long-lived surfaces should call `POST /api/v1/discovery/sessions/{id}/heartbeat` periodically while open.

---

## 6. Transfer State Machine & Idempotency

### Idempotent Outbound Requests

Clients should generate a unique `request_id` (e.g. UUID v4) per user action and send it in `POST /api/v1/transfers`.

- **First call:** Returns HTTP `201 Created` with `"idempotent_replay": false` and a fresh `transfer_id` (`tr_<24hex>`).
- **Repeated call with the same `request_id`:** Returns HTTP `200 OK` with `"idempotent_replay": true` and the **exact same** `transfer_id` and current `transfer` state — **without** starting a second transfer.

### Normalized Transfer States

Raw protocol states (UKEY2 handshake, PairedKeyEncryption, AirDrop `/Ask` & `/Upload`, etc.) are normalized into seven widget-friendly states:

| `state` | Description |
|---|---|
| `connecting` | Establishing link (LAN/BLE/AWDL), TLS/UKEY2 handshake, or AirDrop `/Ask` in flight. |
| `waiting_for_consent` | Waiting for recipient approval (outbound) or local user approval (inbound). |
| `in_progress` | Actively transferring file payload bytes (`bytes_transferred` / `total_bytes` / `progress`). |
| `completed` | Transfer finished cleanly (`progress: 1.0`). |
| `declined` | Rejected by the remote peer or declined locally. |
| `cancelled` | Aborted via cancel endpoint, desktop notification, or remote cancellation. |
| `failed` | Network error, timeout, or unexpected disconnect (`error` contains details when available). |

### `ApiTransfer` Object Schema

```json
{
  "transfer_id": "tr_91a4f7c208b6d3e154ab12cd",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "direction": "outbound",
  "device_id": "dev_8f31c2a94b10e7d5",
  "peer_name": "Pixel 9",
  "protocol": "quickshare",
  "state": "in_progress",
  "pin_code": "4829",
  "files": ["Example.pdf"],
  "saved_paths": [],
  "bytes_transferred": 1048576,
  "total_bytes": 4194304,
  "progress": 0.25,
  "error": null,
  "created_at_ms": 1764081240000,
  "updated_at_ms": 1764081242500
}
```

- `direction`: `"outbound"` or `"inbound"`
- `progress`: floating-point value in `[0.0, 1.0]`
- `saved_paths`: populated with local destination file paths on completed inbound transfers

---

## 7. HTTP & SSE Endpoint Reference

Base URL: `http://127.0.0.1:8787` (read dynamically from `api.json` or D-Bus `GetApiInfo()`).

### `GET /api/v1/health`

Lightweight **unauthenticated** liveness probe.

**Response `200 OK`:**
```json
{
  "ok": true,
  "api_version": "v1",
  "version": "0.3.0"
}
```

---

### `GET /api/v1/status`

Returns daemon status, visibility, AirDrop radio status, and live counts.

**Response `200 OK`:**
```json
{
  "api_version": "v1",
  "version": "0.3.0",
  "device_name": "FedoraByAmare",
  "visibility": "Visible",
  "download_dir": "/home/amare/Downloads",
  "auto_accept": false,
  "qs_port": 41893,
  "airdrop_port": 8771,
  "airdrop": {
    "available": true,
    "active": false,
    "mode": "exclusive",
    "channel": 6,
    "driver": "iwlwifi",
    "device": "8086:a0f0",
    "reason": null
  },
  "active_discovery_sessions": 1,
  "reachable_devices": 2
}
```

---

### `GET /api/v1/devices`

Returns an **immediate snapshot** of all currently reachable merged devices without waiting for new scan events.

**Response `200 OK`:**
```json
{
  "devices": [
    {
      "device_id": "dev_8f31c2a94b10e7d5",
      "name": "Pixel 9",
      "disambiguator": null,
      "device_type": "phone",
      "protocol": "quickshare",
      "transports": ["lan", "ble"],
      "preferred_transport": "lan",
      "reachable": true,
      "last_seen_ms": 1764081234567
    }
  ]
}
```

---

### `POST /api/v1/discovery/sessions`

Starts or joins hardware discovery under an isolated client session.

**Request Body (optional):**
```json
{
  "client": "kde-plasma-island",
  "ttl_seconds": 120
}
```

**Response `201 Created`:**
```json
{
  "session_id": "ds_7c41f0e829a1b3d5c6e7f809",
  "active_sessions": 1,
  "expires_in_seconds": 120
}
```

---

### `GET /api/v1/discovery/sessions`

Lists all currently active (non-expired) discovery sessions.

**Response `200 OK`:**
```json
{
  "active_sessions": 2,
  "sessions": [
    {
      "session_id": "builtin-ui",
      "client": "desktop-ui",
      "expires_in_seconds": 284
    },
    {
      "session_id": "ds_7c41f0e829a1b3d5c6e7f809",
      "client": "kde-plasma-island",
      "expires_in_seconds": 112
    }
  ]
}
```

---

### `POST /api/v1/discovery/sessions/{id}/heartbeat`

Extends an existing discovery session's TTL.

**Request Body (optional):**
```json
{
  "ttl_seconds": 120
}
```

**Response `200 OK`:**
```json
{
  "ok": true,
  "session_id": "ds_7c41f0e829a1b3d5c6e7f809",
  "expires_in_seconds": 120
}
```

Returns `404 SESSION_NOT_FOUND` if the session has expired or was deleted.

---

### `DELETE /api/v1/discovery/sessions/{id}`

Closes a discovery session. Hardware scanning stops **only** if `active_sessions` drops to `0`.

**Response `200 OK`:**
```json
{
  "ok": true,
  "session_id": "ds_7c41f0e829a1b3d5c6e7f809",
  "active_sessions": 0
}
```

Returns `404 SESSION_NOT_FOUND` if unknown/already expired.

---

### `GET /api/v1/transfers`

Lists tracked inbound and outbound transfers sorted by most recently updated first.

**Response `200 OK`:**
```json
{
  "transfers": [
    { "...": "ApiTransfer object" }
  ]
}
```

---

### `POST /api/v1/transfers`

Creates an idempotent outbound file transfer to `device_id`. Directories in `files` are expanded recursively into individual readable files.

**Request Body:**
```json
{
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "device_id": "dev_8f31c2a94b10e7d5",
  "files": [
    "/home/amare/Documents/Example.pdf"
  ]
}
```

**Response `201 Created` (new transfer) or `200 OK` (idempotent replay):**
```json
{
  "transfer_id": "tr_91a4f7c208b6d3e154ab12cd",
  "idempotent_replay": false,
  "transfer": {
    "transfer_id": "tr_91a4f7c208b6d3e154ab12cd",
    "request_id": "550e8400-e29b-41d4-a716-446655440000",
    "direction": "outbound",
    "device_id": "dev_8f31c2a94b10e7d5",
    "peer_name": "Pixel 9",
    "protocol": "quickshare",
    "state": "connecting",
    "pin_code": null,
    "files": ["Example.pdf"],
    "saved_paths": [],
    "bytes_transferred": 0,
    "total_bytes": 4194304,
    "progress": 0.0,
    "error": null,
    "created_at_ms": 1764081240000,
    "updated_at_ms": 1764081240000
  }
}
```

**Possible Errors:** `400 INVALID_REQUEST`, `404 DEVICE_NOT_FOUND`, `409 DEVICE_UNREACHABLE`, `422 EMPTY_FILES`.

---

### `GET /api/v1/transfers/{id}`

Fetches the current state of a single transfer.

**Response `200 OK`:**
```json
{
  "transfer": { "...": "ApiTransfer object" }
}
```

---

### `POST /api/v1/transfers/{id}/cancel` / `DELETE /api/v1/transfers/{id}`

Cancels an active outbound or inbound transfer. Incomplete received files are deleted automatically.

**Response `200 OK`:**
```json
{
  "ok": true,
  "transfer_id": "tr_91a4f7c208b6d3e154ab12cd"
}
```

Returns `404 TRANSFER_NOT_FOUND` if the `transfer_id` is unknown.

---

### `POST /api/v1/transfers/{id}/accept`

Accepts a pending inbound transfer (`state == "waiting_for_consent"`).

**Response `200 OK`:**
```json
{
  "ok": true,
  "transfer_id": "tr_..."
}
```

---

### `POST /api/v1/transfers/{id}/decline`

Declines a pending inbound transfer (`state == "waiting_for_consent"`).

**Response `200 OK`:**
```json
{
  "ok": true,
  "transfer_id": "tr_..."
}
```

---

### `POST /api/v1/visibility`

Changes Quick Share receiver visibility.

**Request Body:**
```json
{
  "visibility": "Visible"
}
```

Accepted values (case-insensitive): `"Visible"`, `"Temporarily"`, `"Invisible"`.

**Response `200 OK`:**
```json
{
  "ok": true,
  "visibility": "Visible"
}
```

---

### `POST /api/v1/airdrop/enable`

Enables the embedded AWDL AirDrop radio (`aethershare-awdld`). On Exclusive-mode Wi-Fi cards (e.g. Intel AX201), Quick Share scanning is paused while AirDrop Mode is active.

**Response `200 OK`:**
```json
{
  "ok": true,
  "airdrop": {
    "available": true,
    "active": true,
    "mode": "exclusive",
    "channel": 6,
    "driver": "iwlwifi",
    "device": "8086:a0f0",
    "reason": null
  }
}
```

Returns `409 AIRDROP_UNAVAILABLE` if the AWDL daemon cannot be started.

---

### `POST /api/v1/airdrop/disable`

Disables the AWDL AirDrop radio (restoring normal Wi-Fi on Exclusive-mode cards) and automatically resumes Quick Share discovery if any discovery sessions remain active.

**Response `200 OK`:**
```json
{
  "ok": true,
  "airdrop": {
    "available": true,
    "active": false,
    "mode": "exclusive",
    "channel": 6,
    "driver": "iwlwifi",
    "device": "8086:a0f0",
    "reason": null
  }
}
```

---

### `GET /api/v1/events` (SSE)

Server-Sent Events stream providing an **immediate initial snapshot** followed by incremental `device` and `transfer` events, plus `: keepalive` comments every 15 seconds.

Pass authentication via `Authorization: Bearer <token>` or `?token=<token>`.

#### Event Types

1. **`event: snapshot`** (emitted immediately upon connecting, subscribed atomically before snapshotting so no intermediate events are lost):
   ```text
   event: snapshot
   data: {"devices":[...],"transfers":[...],"active_discovery_sessions":1}
   ```

2. **`event: device`** (emitted when a device is discovered, its transports/disambiguator change, or it becomes unreachable):
   ```text
   event: device
   data: {"type":"upsert","device":{"device_id":"dev_...","name":"Pixel 9",...}}
   ```
   `type` is `"upsert"` (add or update in widget list) or `"removed"` (device no longer reachable on any transport).

3. **`event: transfer`** (emitted on every state transition and byte-progress update):
   ```text
   event: transfer
   data: {"transfer_id":"tr_...","state":"in_progress","bytes_transferred":524288,"total_bytes":2097152,"progress":0.25,...}
   ```

---

## 8. Widget Integration Guide (KDE Plasma / QML / Shell)

### Recommended Lifecycle for a KDE Plasma Widget / Dynamic Island

1. **Startup / Auto-Activation:**
   - Call `org.aethershare.Service.GetApiInfo()` over Session D-Bus (auto-launches `aethershare-desktop --background` if not running), or read `$XDG_RUNTIME_DIR/aethershare/api.json`.
   - Read the bearer token from `token_file`.
2. **Open SSE Connection Immediately:**
   - Connect to `GET /api/v1/events?token=<token>`.
   - Populate the widget immediately from the first `event: snapshot` (or call `GET /api/v1/devices` on demand).
   - Keep the SSE connection open while the widget is alive so inbound transfers and active progress appear automatically in the Dynamic Island.
3. **When the User Opens the Send Drawer / Drops Files on the Island:**
   - Call `POST /api/v1/discovery/sessions` with `{"client":"kde-island","ttl_seconds":60}`.
   - Store the returned `session_id` and refresh it with `POST /api/v1/discovery/sessions/{id}/heartbeat` every ~30s while the drawer remains open.
   - When the drawer closes, call `DELETE /api/v1/discovery/sessions/{id}` (safe even if the main AetherShare desktop app is also scanning).
4. **Sending Files:**
   - Generate a `request_id` (UUID) per drop/click and call `POST /api/v1/transfers` with `{ request_id, device_id, files }`.
   - Track the returned `transfer_id` in the `event: transfer` stream (`connecting` → `waiting_for_consent` → `in_progress` with `bytes_transferred` / `total_bytes` / `progress` → `completed` / `declined` / `cancelled` / `failed`).
   - Wire the Island's cancel button to `POST /api/v1/transfers/{transfer_id}/cancel`.

### Quick End-to-End `curl` Walkthrough

```bash
# 1. Ensure AetherShare is running via D-Bus auto-activation
busctl --user call org.aethershare.Service /org/aethershare/Service \
  org.aethershare.Service Ping

# 2. Load base URL and access token
API_BASE=$(jq -r .api_base "$XDG_RUNTIME_DIR/aethershare/api.json")
TOKEN=$(cat "$XDG_RUNTIME_DIR/aethershare/api.token")
AUTH="Authorization: Bearer $TOKEN"

# 3. Start a widget discovery session
SESSION_ID=$(curl -sS -X POST "$API_BASE/discovery/sessions" \
  -H "$AUTH" -H "Content-Type: application/json" \
  -d '{"client":"kde-island","ttl_seconds":60}' | jq -r .session_id)

# 4. Fetch immediately available merged devices
curl -sS "$API_BASE/devices" -H "$AUTH" | jq .

# 5. Send a file idempotently
TRANSFER_ID=$(curl -sS -X POST "$API_BASE/transfers" \
  -H "$AUTH" -H "Content-Type: application/json" \
  -d '{
    "request_id": "req-demo-001",
    "device_id": "dev_8f31c2a94b10e7d5",
    "files": ["/home/amare/Documents/Example.pdf"]
  }' | jq -r .transfer_id)

# 6. Stream live events (initial snapshot + progress)
curl -N "$API_BASE/events?token=$TOKEN"

# 7. Cancel if needed
curl -sS -X POST "$API_BASE/transfers/$TRANSFER_ID/cancel" -H "$AUTH"

# 8. Release only this widget's discovery session when closing
curl -sS -X DELETE "$API_BASE/discovery/sessions/$SESSION_ID" -H "$AUTH"
```
