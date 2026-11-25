//! Embedded AWDL radio service for AetherShare (`aethershare-awdld`).
//!
//! Powered by the MIT-licensed `owlyd` and `owly-radio` crates from
//! <https://github.com/t4t5/owly>. Runs with `CAP_NET_ADMIN` + `CAP_NET_RAW`
//! only, exposes the `io.github.t4t5.Owly.Radio1` system D-Bus interface,
//! manages the `awdl0` virtual interface, and automatically restores Wi-Fi
//! when the lease ends or the client disconnects.

use std::process::ExitCode;

use owly_radio::fake::{self, Fake};
use owly_radio::injection::Injection;
use owly_radio::rundir::{RUN_DIR, RunDir};
use owlyd::service::{self, Options};

const USAGE: &str = "usage: aethershare-awdld [--bus ADDRESS] [--run-dir DIR] [--fake ROOT]";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let mut bus = None;
    let mut run_dir = None;
    let mut fake = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let value = match a.as_str() {
            "--bus" => &mut bus,
            "--run-dir" => &mut run_dir,
            "--fake" => &mut fake,
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        };
        match args.next() {
            Some(v) => *value = Some(v),
            None => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");

    let served = match fake {
        Some(root) => {
            let options = Options { bus, polkit: false };
            rt.block_on(service::serve(
                Fake::new(root),
                fake::probe,
                options,
                shutdown(),
            ))
        }
        None => {
            let run = RunDir::new(run_dir.unwrap_or_else(|| RUN_DIR.into()));
            let backend = match Injection::from_config(run.clone()) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!("{e}");
                    return ExitCode::from(1);
                }
            };
            let probe = move || owly_radio::probe::from_config(run.clone());
            let options = Options { bus, polkit: true };
            rt.block_on(service::serve(backend, probe, options, shutdown()))
        }
    };

    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::from(1)
        }
    }
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
