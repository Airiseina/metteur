#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;
use metteur_daemon::cli::Cli;
use mimalloc::MiMalloc;
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    let cli = Cli::parse();

    if cli.install_service {
        return metteur_daemon::service::install_service(&cli).map_err(Into::into);
    }
    if cli.gen_certs {
        return metteur_daemon::cert::run(&cli).map_err(Into::into);
    }
    if cli.gen_signing_key.is_some() || cli.sign_addon.is_some() {
        return metteur_daemon::signer::run(&cli).map_err(Into::into);
    }
    if cli.uninstall_service {
        return metteur_daemon::service::uninstall_service().map_err(Into::into);
    }
    if cli.start_service {
        return metteur_daemon::service::start_service().map_err(Into::into);
    }
    if cli.stop_service {
        return metteur_daemon::service::stop_service().map_err(Into::into);
    }
    if cli.service_status {
        return metteur_daemon::service::service_status().map_err(Into::into);
    }
    if cli.service {
        // Blocks under the SCM dispatcher until the service is stopped.
        return metteur_daemon::service::run_service().map_err(Into::into);
    }

    metteur_daemon::startup::run(cli, None).await
}

/// Initializes the structured logging subscriber.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}