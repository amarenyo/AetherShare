> [!NOTE]
> **Vibecoded with [Antigravity](https://antigravity.google/)** — This project was vibecoded with Antigravity. I just needed a client on my fedora laptop so i just told antigravity to build one. It is pretty good tho.

# AetherShare

**Open-Source Android Quick Share & Apple AirDrop Desktop App + CLI for Linux**

`AetherShare` is a native Rust + Tauri v2 desktop application and CLI suite for seamless local file sharing between Linux, Android (Quick Share / Nearby Connections), and Apple devices (AirDrop) — with zero manual Bluetooth configuration.

## Highlights

- **Native Material 3 Expressive Desktop App (`app/src-tauri` + `app/ui`):**
  - Built with official Google `@material/web` components and `@material/material-color-utilities` (`SchemeVibrant`, 2025 Expressive spec).
  - Live **System Accent Color & Light/Dark Theme** sync on KDE Plasma, GNOME, and other Linux DEs via XDG Desktop Portal (`org.freedesktop.portal.Settings`, with `~/.config/kdeglobals` fallback).
  - **Automatic DE Language Detection (German / English):** Detects locale across KDE Plasma (`plasma-localerc`), GNOME, COSMIC, XFCE, Hyprland, Sway (`LC_ALL`/`LC_MESSAGES`/`LANGUAGE`/`LANG`, `locale.conf`) with optional manual override in Settings.
  - Clean, beginner-friendly UI: combined Quick Share + AirDrop device list, focus-aware discovery with animated reload/radar indicator, drag-and-drop file & folder sending, system tray background receiver, and technical diagnostics tucked under *Developer Information*.
- **Interactive Desktop Notifications & Safe Cancel Cleanup:**
  - Incoming transfer notifications with direct **Accept / Decline** action buttons.
  - Native **KDE Plasma 6 progress bar** (`org.kde.kuiserver` / `org.kde.JobViewV2`) with Cancel button (plus `notify-send` progress fallback on other DEs) and **Open** button on completion.
  - Automatic deletion of incomplete files whenever a transfer is cancelled or disconnected from either side.
- **Full Android Quick Share Compatibility (even when Android drops Wi-Fi while scanning):**
  - **BLE Receiver Discovery (`0xFEF3`)**: Dual Extended + 15-byte Legacy Header (MurmurHash3 Bloom filter for `"NearbySharing"`).
  - **BLE L2CAP CoC & GATT Weave Socket (`0101`/`0102`)**: Performs connection setup and UKEY2 handshake directly over Bluetooth without requiring pairing.
  - **Automatic Bandwidth Upgrade (`WIFI_LAN`, `WIFI_DIRECT`, `WIFI_HOTSPOT`)**: Upgrades seamlessly to fast LAN TCP or temporary NetworkManager Direct/Hotspot links.
- **Smart Network Interface Filtering (`aethershare-core::net_filter`):**
  - Automatically excludes virtual/container/VPN interfaces (`vmnet*`, `docker*`, `virbr*`, `veth*`, `tun*`, `tailscale*`, `wg*`) from mDNS and bandwidth upgrades.
  - Normalizes mDNS hostnames to valid `<hostname>.local.` names and handles IP changes / suspend-resume automatically.
- **Built-in Apple AirDrop Engine (`aethershare-airdrop`):**
  - Apple Continuity BLE AirDrop Beacon (`0x004C`, type `0x05`).
  - `_airdrop._tcp.local.` DNS-SD advertisement & discovery.
  - HTTPS/TLS + HTTP/1.1 Server & Client for `/Discover`, `/Ask`, and `/Upload` (Binary/XML Plist + pure-Rust POSIX.1 `070707` ODC CPIO + Gzip).
- **CLI & Headless Daemon (`aethershare-cli`):**
  - `aethershare doctor` — System, interface, Bluetooth & mDNS diagnostics
  - `aethershare ui` — Launches the local daemon + embedded web UI (`http://127.0.0.1:8787`)
  - `aethershare daemon` — Headless background receiver with interactive desktop notifications
  - `aethershare devices` — Scans for nearby Quick Share (LAN + BLE) & AirDrop devices
  - `aethershare send <TARGET> <FILE...>` — Sends files or folders via Quick Share or AirDrop

## Project Structure

- `crates/aethershare-proto` — Wire protocol definitions (Protobuf, UKEY2, SecureMessage, P-256 crypto)
- `crates/aethershare-core` — Quick Share engine (mDNS, BLE `0xFEF3`, L2CAP CoC, GATT Weave, Wi-Fi BWU, network filtering, interactive notifications, cross-DE i18n)
- `crates/aethershare-airdrop` — Apple AirDrop engine (Continuity BLE, mDNS, TLS/HTTP, Plist, CPIO+Gzip)
- `crates/aethershare-cli` — CLI tool & embedded web UI server
- `app/ui` — React + TypeScript + `@material/web` frontend (bundled via Vite)
- `app/src-tauri` — Native Tauri v2 Linux desktop app with system tray & OS drag-and-drop

## Building from Source

### 1. System Prerequisites

**Fedora:**
```bash
sudo dnf install rust cargo nodejs npm protobuf-compiler dbus-devel \
  webkit2gtk4.1-devel gtk3-devel libsoup3-devel librsvg2-devel \
  libayatana-appindicator-gtk3-devel
```

**Ubuntu / Debian:**
```bash
sudo apt install cargo nodejs npm protobuf-compiler libdbus-1-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev \
  libayatana-appindicator3-dev
```

### 2. Build the Frontend UI (Required First)

```bash
cd app/ui
npm install
npm run build
cd ../..
```

### 3. Build & Test Rust Workspace + CLI

```bash
cargo test --workspace
cargo build --release -p aethershare-cli
```

### 4. Build the Native Desktop App

```bash
cd app/src-tauri
cargo build --release
./target/release/aethershare-desktop
```

## License

- Core crates (`aethershare-proto`, `aethershare-core`, `aethershare-airdrop`, `aethershare-cli`): `Apache-2.0 OR MIT`
- Desktop application (`app/src-tauri`): `GPL-3.0-or-later`
