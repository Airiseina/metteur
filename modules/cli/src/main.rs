//! The `metteur` CLI binary.

use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use metteur_cli::{daemon_spawn, repl, session, tui};

/// Metteur command-line client.
#[derive(Debug, Parser)]
#[command(name = "metteur", version, about)]
struct Cli {
    /// Daemon gRPC address.
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    addr: String,

    /// CA used to verify the daemon (PEM). Requires `--tls-cert`/`--tls-key`.
    #[arg(long)]
    tls_ca: Option<std::path::PathBuf>,

    /// Client certificate presented to the daemon (PEM).
    #[arg(long)]
    tls_cert: Option<std::path::PathBuf>,

    /// Client private key (PEM).
    #[arg(long)]
    tls_key: Option<std::path::PathBuf>,

    /// Workspace opened immediately after connecting.
    #[arg(long)]
    workspace: Option<String>,

    /// Force the line-based REPL instead of the TUI (for shell pipelines).
    #[arg(long)]
    no_tui: bool,

    /// Disable automatically starting the daemon when it is unreachable.
    #[arg(long)]
    no_spawn: bool,

    /// Path to the `metteurd` binary used by automatic startup.
    #[arg(long)]
    daemon_binary: Option<std::path::PathBuf>,

    /// Daemon global data directory passed to automatic startup.
    #[arg(long)]
    daemon_data_dir: Option<std::path::PathBuf>,

    /// Daemon config file passed to automatic startup.
    #[arg(long)]
    daemon_config: Option<std::path::PathBuf>,

    /// Daemon wake socket/pipe passed to automatic startup.
    #[arg(long)]
    daemon_wake_path: Option<std::path::PathBuf>,

    /// Daemon PID file passed to automatic startup.
    #[arg(long)]
    daemon_pid_file: Option<std::path::PathBuf>,

    /// Daemon server TLS certificate forwarded to automatic startup.
    #[arg(long)]
    daemon_tls_cert: Option<std::path::PathBuf>,

    /// Daemon server TLS private key forwarded to automatic startup.
    #[arg(long)]
    daemon_tls_key: Option<std::path::PathBuf>,

    /// Daemon client CA forwarded to automatic startup.
    #[arg(long)]
    daemon_tls_client_ca: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let tls = session::TlsPaths::from_args(
        cli.tls_ca.clone(),
        cli.tls_cert.clone(),
        cli.tls_key.clone(),
    )?;
    let client = match session::connect(&cli.addr, tls.as_ref()).await {
        Ok(client) => client,
        Err(first_error) => {
            if cli.no_spawn {
                return Err(first_error.context("daemon unreachable and --no-spawn is set"));
            }
            ensure_daemon(&cli).await.context("automatic daemon startup failed")?;
            session::connect(&cli.addr, tls.as_ref())
                .await
                .context("daemon still unreachable after automatic startup")?
        }
    };

    let interactive = !cli.no_tui && std::io::IsTerminal::is_terminal(&std::io::stdout());
    if interactive {
        tui::run_tui(client, cli.workspace).await
    } else {
        repl::run(client, cli.workspace).await
    }
}

/// Wakes an already-running daemon, or launches a detached one and wakes it,
/// then waits until its gRPC endpoint is ready.
async fn ensure_daemon(cli: &Cli) -> anyhow::Result<()> {
    let listen_addr = daemon_spawn::listen_from_addr(&cli.addr)?;
    let wake_path = cli
        .daemon_wake_path
        .clone()
        .unwrap_or_else(daemon_spawn::default_wake_path);

    // A daemon registered to auto-start may already be running quietly; waking
    // it alone avoids launching a duplicate.
    if daemon_spawn::send_wake(&wake_path).await.is_ok() {
        return daemon_spawn::wait_ready(
            &listen_addr,
            Duration::from_secs(15),
            Duration::from_millis(250),
        )
        .await;
    }
    if cli.no_spawn {
        anyhow::bail!("daemon unreachable and no wake listener present (--no-spawn)");
    }

    let binary = daemon_spawn::locate_daemon(cli.daemon_binary.as_deref())?;
    let spawn = daemon_spawn::DaemonSpawn {
        binary,
        listen_addr,
        data_dir: cli.daemon_data_dir.clone(),
        config: cli.daemon_config.clone(),
        wake_path: Some(wake_path.clone()),
        pid_file: cli.daemon_pid_file.clone(),
        tls_cert: cli.daemon_tls_cert.clone(),
        tls_key: cli.daemon_tls_key.clone(),
        tls_client_ca: cli.daemon_tls_client_ca.clone(),
    };
    daemon_spawn::spawn_daemon(&spawn)?;
    daemon_spawn::wait_woke_and_ready(
        &spawn.listen_addr,
        &wake_path,
        Duration::from_secs(15),
        Duration::from_millis(250),
    )
    .await
}
