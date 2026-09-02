//! The daemon startup pipeline, shared between the plain binary and the
//! Windows service entry point.
//!
//! Order: parse CLI (caller) -> load global config -> detach into an
//! independent process -> register login autostart when configured -> wait
//! for a wake signal (passive mode) -> serve gRPC. An optional `shutdown`
//! channel lets the Windows SCM stop the daemon at any stage.

pub mod autostart;
pub mod process;
pub mod service;

use std::sync::Arc;

use crate::cli::Cli;
use crate::config;
use crate::error::DaemonError;
use crate::grpc::AppState;
use crate::registry::Registry;
use crate::workspace::WorkspaceManager;

/// Runs the daemon to completion.
///
/// `shutdown`, when present, is observed during the passive wait and the gRPC
/// serve phase: once it turns `true` the daemon stops cleanly instead of
/// waiting forever. Used by the Windows service entry point.
pub async fn run(
    cli: Cli,
    shutdown: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let global_config_path = match &cli.config {
        Some(path) => {
            if !path.exists() {
                return Err(format!("config file not found: {}", path.display()).into());
            }
            path.clone()
        }
        None => config::default_global_config_path()?,
    };
    let global_config = config::load_global_config(&global_config_path)?;

    // Re-exec as an independent background process and let the parent exit.
    if cli.detach {
        let pid_file =
            cli.pid_file.clone().unwrap_or(config::global_config_dir()?.join("daemon.pid"));
        crate::startup::process::detach::detach(&pid_file)?;
        return Ok(());
    }

    // `[daemon].autostart` registers the daemon to start with the host OS.
    // A login entry puts the daemon into passive listening; the `service` mode
    // registers the daemon as a Windows service (idempotent, so registration
    // only happens once).
    match global_config.daemon.autostart {
        metteur_shared::config::AutostartMode::Off => {}
        metteur_shared::config::AutostartMode::Login => {
            crate::startup::autostart::install(&crate::startup::autostart::AutostartEnv {
                exe: std::env::current_exe()?,
                config: cli.config.clone(),
                data_dir: cli.data_dir.clone(),
            })?;
        }
        metteur_shared::config::AutostartMode::Service => {
            if !crate::startup::service::is_installed()? {
                crate::startup::service::install_service(&cli)?;
            }
        }
    }

    // `[daemon].wake` makes passive mode the default unless `--passive` is
    // already controlling it; in passive mode the daemon waits for a wake
    // signal before fully starting.
    let passive = cli.passive || global_config.daemon.wake;
    if passive {
        let wake_path =
            cli.wake_path.clone().unwrap_or_else(crate::wake::default_wake_path);
        tracing::info!("passive mode: waiting for wake signal");
        let wake_fut = crate::wake::wait_for_wake(&wake_path);
        if let Some(mut shutdown) = shutdown.as_ref().cloned() {
            tokio::select! {
                result = wake_fut => result?,
                _ = shutdown.changed() => return Ok(()),
            }
        } else {
            wake_fut.await?;
        }
    }

    let listen_addr = resolve_listen(cli.listen_addr, &global_config.daemon.listen_addr)?;
    tracing::info!("starting metteurd on {}", listen_addr);

    let workspaces = WorkspaceManager::new().with_global_config_path(global_config_path);
    let registry = Registry::with_builtins();

    let tls = match (&cli.tls_cert, &cli.tls_key, &cli.tls_client_ca) {
        (Some(cert), Some(key), Some(ca)) => {
            tracing::info!("enabling mutual TLS");
            Some(crate::tls::load(cert, key, ca)?)
        }
        (None, None, None) => None,
        _ => {
            return Err(
                "--tls-cert, --tls-key and --tls-client-ca must be provided together".into()
            );
        }
    };

    let data_dir =
        cli.data_dir.clone().unwrap_or(crate::config::global_config_dir()?);
    let global_db = crate::storage::persistence::Db::open(&data_dir.join("db"))?;

    let registry = Arc::new(registry);
    let metrics = Arc::new(crate::observability::metrics::Metrics::default());
    let mcp_host = crate::integration::mcp::McpHost::new(registry.clone(), metrics.clone());
    // Initial sync before the server accepts requests; failures are isolated
    // per server and reported through `ListMcpServers`.
    mcp_host.sync(&global_config.mcp).await;

    let addon_host = crate::addon::AddonHost::new(
        &data_dir,
        registry.clone(),
        global_config.addon.call_timeout_ms,
        &global_config.addon,
    );

    let state = Arc::new(
        AppState::new(workspaces, registry, global_config)
            .with_global_db(global_db)
            .with_metrics(metrics)
            .with_mcp_host(mcp_host)
            .with_addon_host(addon_host)
            .await,
    );

    if let Some(metrics_addr) = cli.metrics_addr {
        let metrics = state.metrics.clone();
        tokio::spawn(async move {
            if let Err(err) = crate::observability::metrics::http::serve(metrics_addr, metrics).await {
                tracing::error!("metrics server failed: {err}");
            }
        });
    }

    let serve_fut = crate::grpc::serve(listen_addr, state, tls);
    let to_box = |e: DaemonError| -> Box<dyn std::error::Error> { Box::new(e) };
    match shutdown {
        Some(mut shutdown) => {
            tokio::select! {
                result = serve_fut => result.map_err(to_box),
                _ = shutdown.changed() => Ok(()),
            }
        }
        None => serve_fut.await.map_err(to_box),
    }
}

/// Resolves the gRPC listen address, honouring the `[daemon].listen_addr`
/// override only when the CLI kept the built-in default.
fn resolve_listen(
    cli_addr: std::net::SocketAddr,
    override_addr: &Option<String>,
) -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
    const DEFAULT_LISTEN: std::net::SocketAddr =
        std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 50051);
    if cli_addr != DEFAULT_LISTEN {
        return Ok(cli_addr);
    }
    match override_addr {
        Some(addr) => addr.parse().map_err(|e| format!("invalid [daemon].listen_addr {addr:?}: {e}").into()),
        None => Ok(cli_addr),
    }
}