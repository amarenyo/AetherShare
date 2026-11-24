use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aethershare_airdrop::{
    AirDropClient, AirDropMdnsDiscovery, AirDropMdnsServer, AirDropServer,
};
use aethershare_core::channel::{Message, TransferKind};
use aethershare_core::net_filter::{
    diagnose_interfaces, filtered_ipv4_addrs, normalize_mdns_hostname, primary_lan_ipv4,
};
use aethershare_core::{EndpointInfo, OutboundPayload, RQS, SendInfo, TransferState, Visibility};
use log::{error, info, warn};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

mod web_ui;

fn print_usage() {
    println!(
        r#"AetherShare v0.1.0 — Open-Source Quick Share & AirDrop Suite for Linux

USAGE:
    aethershare <COMMAND> [OPTIONS]

COMMANDS:
    doctor                  Prüft Netzwerk-Interfaces (vmnet/docker Filter), Bluetooth, mDNS & AirDrop
    daemon                  Startet den Quick Share (mDNS + BLE + L2CAP + Wi-Fi Direct) & AirDrop Daemon
    ui                      Startet den Daemon inkl. Desktop/Web-UI (http://127.0.0.1:8787) und öffnet das Fenster
    devices [--timeout N]   Sucht nach Quick Share (WLAN + BLE) und AirDrop Geräten in der Nähe
    send <ZIEL> <DATEI...>  Sendet Dateien per Quick Share (WLAN / BLE+Wi-Fi Direct) oder AirDrop

OPTIONS FÜR 'daemon' / 'ui':
    --name <NAME>           Gerätename (Standard: System-Hostname)
    --port <PORT>           Fester Quick Share TCP-Port (Standard: automatisch)
    --airdrop-port <PORT>   AirDrop HTTPS/TLS Port (Standard: 8770)
    --web-port <PORT>       Lokaler Web-UI/Tauri-Bridge Port (Standard: 8787)
    --download-dir <PFAD>   Zielordner für empfangene Dateien (Standard: ~/Downloads)
    --auto-accept           Eingehende Transfers automatisch akzeptieren
"#
    );
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    if std::env::var("RUST_LOG").is_err() {
        unsafe {
            std::env::set_var("RUST_LOG", "info");
        }
    }
    env_logger::Builder::from_default_env()
        .format_timestamp_secs()
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");

    match cmd {
        "doctor" => run_doctor().await,
        "daemon" => run_daemon(&args[1..], false).await,
        "ui" => run_daemon(&args[1..], true).await,
        "devices" => run_devices(&args[1..]).await,
        "send" => run_send(&args[1..]).await,
        _ => {
            print_usage();
            Ok(())
        }
    }
}

async fn run_doctor() -> Result<(), anyhow::Error> {
    let hostname = sys_metrics::host::get_hostname().unwrap_or_else(|_| "LinuxPC".to_string());
    let mdns_fqdn = normalize_mdns_hostname(&hostname);

    println!("================================================================");
    println!("  AetherShare System-Diagnose (Fedora / Linux)");
    println!("================================================================");
    println!("  Hostname          : {hostname}");
    println!("  mDNS Host-FQDN    : {mdns_fqdn}");

    match primary_lan_ipv4() {
        Some(ip) => println!(
            "  Primäre LAN-IPv4  : {}.{}.{}.{} (wird für Quick Share, BWU & AirDrop genutzt)",
            ip[0], ip[1], ip[2], ip[3]
        ),
        None => println!("  Primäre LAN-IPv4  : KEINE (nur Bluetooth / Wi-Fi Direct Hotspot verfügbar)"),
    }

    println!("\n--- 1. Netzwerk-Interfaces & Smart-Filter ---");
    for diag in diagnose_interfaces() {
        let status = if diag.allowed { "AKTIV  " } else { "FILTER " };
        let fam = if diag.is_ipv6 { "IPv6" } else { "IPv4" };
        println!(
            "  [{status}] {:<14} {:<4} {:<26} -> {}",
            diag.name, fam, diag.ip, diag.reason
        );
    }

    println!("\n--- 2. Bluetooth (BLE 0xFEF3 / L2CAP CoC / GATT Weave) ---");
    #[cfg(target_os = "linux")]
    {
        match aethershare_core::hdl::L2capServer::bind().await {
            Ok((_srv, psm)) => {
                println!("  [OK] BlueZ Adapter erreichbar & L2CAP CoC Server testweise auf PSM {psm} gebunden");
            }
            Err(e) => {
                println!("  [WARN] BlueZ L2CAP CoC Test fehlgeschlagen: {e}");
            }
        }
    }

    println!("\n--- 3. NetworkManager (Wi-Fi Direct / Hotspot Bandwidth Upgrade) ---");
    match tokio::process::Command::new("nmcli")
        .args(["-t", "-f", "DEVICE,TYPE,STATE", "device"])
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if line.contains(":wifi:") {
                    println!("  [OK] WLAN-Interface für Hotspot/Direct-Upgrade gefunden: {line}");
                }
            }
        }
        _ => println!("  [WARN] nmcli nicht erreichbar"),
    }

    println!("\n--- 4. AirDrop Engine (_airdrop._tcp.local. + ODC CPIO + TLS) ---");
    let allowed_ips = filtered_ipv4_addrs();
    println!(
        "  [OK] AirDrop mDNS IPs: {:?} (vmnet/docker ausgeschlossen)",
        allowed_ips
    );
    println!("================================================================");
    Ok(())
}

async fn run_daemon(args: &[String], open_browser: bool) -> Result<(), anyhow::Error> {
    let mut device_name =
        sys_metrics::host::get_hostname().unwrap_or_else(|_| "AetherShare".to_string());
    let mut qs_port: Option<u32> = None;
    let mut airdrop_port: u16 = 8770;
    let mut web_port: u16 = 8787;
    let mut download_dir: Option<PathBuf> = None;
    let mut auto_accept = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" if i + 1 < args.len() => {
                device_name = args[i + 1].clone();
                i += 2;
            }
            "--port" if i + 1 < args.len() => {
                qs_port = args[i + 1].parse().ok();
                i += 2;
            }
            "--airdrop-port" if i + 1 < args.len() => {
                airdrop_port = args[i + 1].parse().unwrap_or(8770);
                i += 2;
            }
            "--web-port" if i + 1 < args.len() => {
                web_port = args[i + 1].parse().unwrap_or(8787);
                i += 2;
            }
            "--download-dir" if i + 1 < args.len() => {
                download_dir = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--auto-accept" => {
                auto_accept = true;
                i += 1;
            }
            _ => i += 1,
        }
    }

    info!("Starting AetherShare Daemon as '{device_name}'...");
    info!(
        "Filtered LAN IPv4 addresses: {:?} (virtual interfaces excluded)",
        filtered_ipv4_addrs()
    );

    let mut rqs = RQS::new(
        Visibility::Visible,
        qs_port,
        download_dir,
        Some(device_name.clone()),
    );
    let (send_tx, _ble_rx) = rqs.run().await?;
    let msg_tx = rqs.message_sender.clone();
    let vis_tx = rqs.visibility_sender.clone();

    let ctk = CancellationToken::new();

    // Start AirDrop Server + AirDrop mDNS announcement
    let ad_server =
        AirDropServer::bind(airdrop_port, device_name.clone(), msg_tx.clone(), auto_accept)
            .await?;
    let actual_ad_port = ad_server.local_port()?;
    let ad_ctk = ctk.clone();
    tokio::spawn(async move {
        if let Err(e) = ad_server.run(ad_ctk).await {
            error!("AirDropServer error: {e}");
        }
    });

    if let Ok(ad_mdns) = AirDropMdnsServer::new(
        format!("aethershare-{:04x}", rand::random::<u16>()),
        device_name.clone(),
        actual_ad_port,
    ) {
        let ad_mdns_ctk = ctk.clone();
        tokio::spawn(async move {
            if let Err(e) = ad_mdns.run(ad_mdns_ctk).await {
                warn!("AirDropMdnsServer ended: {e}");
            }
        });
    }

    // Interactive desktop notifications ([Annehmen]/[Ablehnen], native KDE JobViewV2 progress bar, [Öffnen])
    let auto_accept_flag = Arc::new(AtomicBool::new(auto_accept));
    aethershare_core::notifications::spawn_transfer_notifications(
        msg_tx.clone(),
        auto_accept_flag.clone(),
    );

    // Shared discovery channel for Web UI
    let (disc_tx, _) = broadcast::channel::<EndpointInfo>(64);
    let rqs_shared = Arc::new(Mutex::new(rqs));

    let web_url = format!("http://127.0.0.1:{web_port}");
    info!("AetherShare Web-UI & API läuft auf: {web_url}");

    let web_ctk = ctk.clone();
    let rqs_for_web = rqs_shared.clone();
    let auto_accept_for_web = auto_accept_flag.clone();
    tokio::spawn(async move {
        if let Err(e) = web_ui::serve_web_ui(
            web_port,
            device_name,
            actual_ad_port,
            rqs_for_web,
            msg_tx,
            disc_tx,
            send_tx,
            vis_tx,
            auto_accept_for_web,
            web_ctk,
        )
        .await
        {
            error!("Web UI server error: {e}");
        }
    });

    if open_browser {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = tokio::process::Command::new("xdg-open")
            .arg(&web_url)
            .spawn();
    }

    tokio::signal::ctrl_c().await?;
    info!("Beende AetherShare Daemon...");
    ctk.cancel();
    rqs_shared.lock().unwrap().stop().await;
    Ok(())
}

async fn run_devices(args: &[String]) -> Result<(), anyhow::Error> {
    let mut timeout_secs = 8u64;
    if let Some(pos) = args.iter().position(|a| a == "--timeout") {
        if let Some(val) = args.get(pos + 1) {
            timeout_secs = val.parse().unwrap_or(8);
        }
    }

    println!("Suche für {timeout_secs}s nach Quick Share (mDNS + BLE 0xFEF3) & AirDrop Geräten...");
    let (tx, mut rx) = broadcast::channel::<EndpointInfo>(64);
    let ctk = CancellationToken::new();

    if let Ok(qs_mdns) = aethershare_core::hdl::MDnsDiscovery::new(tx.clone()) {
        let c = ctk.clone();
        tokio::spawn(async move {
            let _ = qs_mdns.run(c).await;
        });
    }
    if let Ok(ad_mdns) = AirDropMdnsDiscovery::new(tx.clone()) {
        let c = ctk.clone();
        tokio::spawn(async move {
            let _ = ad_mdns.run(c).await;
        });
    }
    #[cfg(target_os = "linux")]
    {
        let ble_tx = tx.clone();
        let c = ctk.clone();
        tokio::spawn(async move {
            aethershare_core::hdl::ble_discovery(ble_tx, c).await;
        });
    }

    let mut seen: HashMap<String, EndpointInfo> = HashMap::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            Ok(ei) = rx.recv() => {
                if ei.present == Some(true) {
                    let proto = ei.protocol.clone().unwrap_or_else(|| {
                        if ei.id.starts_with("ble://") {
                            "quickshare-ble".to_string()
                        } else {
                            "quickshare-lan".to_string()
                        }
                    });
                    if !seen.contains_key(&ei.id) {
                        println!(
                            "  + [{proto:<14}] Name: {:<22} ID/Addr: {}",
                            ei.name.as_deref().unwrap_or("Unbekannt"),
                            ei.id
                        );
                    }
                    seen.insert(ei.id.clone(), ei);
                }
            }
        }
    }

    ctk.cancel();
    println!("Gefundene Geräte insgesamt: {}", seen.len());
    Ok(())
}

async fn run_send(args: &[String]) -> Result<(), anyhow::Error> {
    if args.len() < 2 {
        println!("Verwendung: aethershare send <ZIEL_ID_ODER_NAME> <DATEI1> [DATEI2...]");
        println!("Beispiele:");
        println!("  aethershare send \"ble://Pixel 9 Pro\" ./bild.jpg");
        println!("  aethershare send 192.168.1.173:53601 ./bild.jpg");
        println!("  aethershare send airdrop://192.168.1.50:8770 ./bild.jpg");
        return Ok(());
    }

    let target = &args[0];
    let file_paths: Vec<PathBuf> = args[1..].iter().map(PathBuf::from).collect();
    for p in &file_paths {
        if !p.is_file() {
            anyhow::bail!("Datei existiert nicht: {:?}", p);
        }
    }

    let hostname = sys_metrics::host::get_hostname().unwrap_or_else(|_| "AetherShare".to_string());

    if let Some(ad_addr) = target.strip_prefix("airdrop://") {
        info!("Sende {} Datei(en) per AirDrop an {ad_addr}...", file_paths.len());
        let client = AirDropClient::new(hostname, true);
        match client.send_files(ad_addr, &file_paths).await {
            Ok(resp) => {
                println!(
                    "AirDrop-Transfer an '{}' erfolgreich abgeschlossen!",
                    resp.receiver_computer_name
                );
                return Ok(());
            }
            Err(_) => {
                let plain_client = AirDropClient::new(
                    sys_metrics::host::get_hostname().unwrap_or_else(|_| "AetherShare".to_string()),
                    false,
                );
                let resp = plain_client.send_files(ad_addr, &file_paths).await?;
                println!(
                    "AirDrop-Transfer an '{}' erfolgreich abgeschlossen!",
                    resp.receiver_computer_name
                );
                return Ok(());
            }
        }
    }

    let mut rqs = RQS::new(Visibility::Invisible, None, None, Some(hostname));
    let (send_tx, _) = rqs.run().await?;
    let mut msg_rx = rqs.message_sender.subscribe();

    let is_ble = target.starts_with("ble://") || !target.contains(':');
    let peer_name = target.strip_prefix("ble://").unwrap_or(target).to_string();
    let id = if is_ble {
        format!("ble://{peer_name}")
    } else {
        target.clone()
    };

    let files_str: Vec<String> = file_paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    send_tx
        .send(SendInfo {
            id: id.clone(),
            name: peer_name,
            addr: target.clone(),
            ob: OutboundPayload::Files(files_str),
            ble: is_ble,
        })
        .await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                anyhow::bail!("Zeitüberschreitung beim Senden");
            }
            Ok(cm) = msg_rx.recv() => {
                if let Message::Client(c) = cm.msg {
                    if c.kind == TransferKind::Outbound {
                        match c.state {
                            Some(TransferState::WaitingForUserConsent) | Some(TransferState::SentIntroduction) => {
                                let pin = c.metadata.as_ref().and_then(|m| m.pin_code.as_deref()).unwrap_or("-");
                                println!("Warte auf Bestätigung am Empfängergerät (PIN: {pin})...");
                            }
                            Some(TransferState::SendingFiles) => {
                                if let Some(m) = c.metadata {
                                    println!("Sende... {}/{} Bytes", m.ack_bytes, m.total_bytes);
                                }
                            }
                            Some(TransferState::Finished) => {
                                println!("Quick Share Transfer erfolgreich abgeschlossen!");
                                break;
                            }
                            Some(TransferState::Rejected) => {
                                anyhow::bail!("Transfer wurde vom Empfänger abgelehnt");
                            }
                            Some(TransferState::Cancelled) | Some(TransferState::Disconnected) => {
                                anyhow::bail!("Transfer abgebrochen oder Verbindung getrennt");
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    rqs.stop().await;
    Ok(())
}
