//! Passive daemon startup from the CLI.
//!
//! When the daemon is unreachable the CLI first pokes its wake listener; if a
//! daemon registered to auto-start is already running (quietly), the wake alone
//! brings it up. Otherwise the CLI launches a detached `metteurd` as an
//! independent background process, wakes it, and waits until its gRPC port is
//! ready before proceeding with the normal client flow.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::{env, io};

use anyhow::Context;

/// The file name of the daemon binary, platform-dependent.
const DAEMON_NAME: &str = if cfg!(windows) { "metteurd.exe" } else { "metteurd" };

/// Everything needed to launch a detached `metteurd` process.
#[derive(Debug, Clone)]
pub struct DaemonSpawn {
    /// The `metteurd` binary to launch.
    pub binary: PathBuf,
    /// Socket address the daemon should listen on, derived from `--addr`.
    pub listen_addr: SocketAddr,
    /// Daemon global data directory, forwarded verbatim.
    pub data_dir: Option<PathBuf>,
    /// Daemon config file, forwarded verbatim.
    pub config: Option<PathBuf>,
    /// Wake socket/pipe the daemon should listen on, forwarded verbatim.
    pub wake_path: Option<PathBuf>,
    /// PID file the detached daemon writes, forwarded verbatim.
    pub pid_file: Option<PathBuf>,
    /// Server TLS certificate, key and client CA, forwarded as a set.
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub tls_client_ca: Option<PathBuf>,
}

/// The default wake socket path (Unix) or pipe name (Windows), matching the
/// daemon's `wake::default_wake_path`.
pub fn default_wake_path() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"\\.\pipe\metteur-wake")
    }
    #[cfg(not(windows))]
    {
        env::temp_dir().join("metteur-wake.sock")
    }
}

/// Locates the `metteurd` binary, honouring `explicit`, then `METTEURD_BIN`,
/// then the directory of the running executable, and finally `PATH`.
pub fn locate_daemon(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(bin) = explicit {
        if bin.is_file() {
            return Ok(bin.to_path_buf());
        }
        anyhow::bail!("daemon binary not found: {}", bin.display());
    }

    if let Ok(bin) = env::var("METTEURD_BIN") {
        let path = PathBuf::from(bin);
        if path.is_file() {
            return Ok(path);
        }
    }

    if let Ok(exe) = env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(DAEMON_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    find_on_path(DAEMON_NAME).with_context(|| {
        format!("cannot locate {DAEMON_NAME}; set --daemon-binary or METTEURD_BIN")
    })
}

/// Builds the command that launches the daemon as a detached, passively
/// running background process.
pub fn build_daemon_command(spawn: &DaemonSpawn) -> Command {
    let mut cmd = Command::new(&spawn.binary);
    cmd.arg("--listen-addr").arg(spawn.listen_addr.to_string());
    if let Some(dir) = &spawn.data_dir {
        cmd.arg("--data-dir").arg(dir);
    }
    if let Some(config) = &spawn.config {
        cmd.arg("--config").arg(config);
    }
    if let Some(wake_path) = &spawn.wake_path {
        cmd.arg("--wake-path").arg(wake_path);
    }
    if let Some(pid_file) = &spawn.pid_file {
        cmd.arg("--pid-file").arg(pid_file);
    }
    if let (Some(cert), Some(key), Some(ca)) =
        (&spawn.tls_cert, &spawn.tls_key, &spawn.tls_client_ca)
    {
        cmd.arg("--tls-cert").arg(cert);
        cmd.arg("--tls-key").arg(key);
        cmd.arg("--tls-client-ca").arg(ca);
    }
    // The daemon must run as an independent background process: wait for a
    // wake signal before serving, and detach from the calling terminal.
    cmd.arg("--passive");
    cmd.arg("--detach");
    cmd
}

/// Launches the daemon as a detached process and returns once it is spawned.
///
/// The client does not own the daemon: dropping the child handle leaves the
/// process running, so the CLI exit never kills it.
pub fn spawn_daemon(spawn: &DaemonSpawn) -> anyhow::Result<()> {
    let mut cmd = build_daemon_command(spawn);
    cmd.spawn()
        .map(|_| ())
        .context("failed to spawn daemon")
}

/// Polls the given address until it accepts TCP connections or `timeout` lapses.
pub async fn wait_ready(
    addr: &SocketAddr,
    timeout: Duration,
    poll: Duration,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tcp_reachable(addr).await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("daemon did not become ready within {}s", timeout.as_secs());
        }
        tokio::time::sleep(poll).await;
    }
}

/// Sends `WAKE` to the daemon's wake listener at `path`.
///
/// Returns `Ok(())` when a passive daemon accepted the signal and is now
/// initializing; returns an error when no listener is present.
pub async fn send_wake(path: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        send_wake_windows(path).await
    }
    #[cfg(not(windows))]
    {
        send_wake_unix(path).await
    }
}

/// Wakes a freshly spawned daemon (retrying until its listener exists) and
/// waits for its gRPC port to become reachable, bounding the total wait by
/// `timeout`.
pub async fn wait_woke_and_ready(
    addr: &SocketAddr,
    wake_path: &Path,
    timeout: Duration,
    poll: Duration,
) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut woke = false;
    loop {
        if !woke && send_wake(wake_path).await.is_ok() {
            woke = true;
        } else if woke && tcp_reachable(addr).await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("daemon did not become ready within {}s", timeout.as_secs());
        }
        tokio::time::sleep(poll).await;
    }
}

#[cfg(windows)]
async fn send_wake_windows(path: &Path) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ClientOptions;

    let mut client = ClientOptions::new().open(path).context("no wake listener")?;
    client.write_all(b"WAKE").await.context("failed to send WAKE")?;
    let mut buf = [0u8; 16];
    let _ = client.read(&mut buf).await;
    Ok(())
}

#[cfg(not(windows))]
async fn send_wake_unix(path: &Path) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let mut stream = UnixStream::connect(path).await.context("no wake listener")?;
    stream.write_all(b"WAKE").await.context("failed to send WAKE")?;
    let mut buf = [0u8; 16];
    let _ = stream.read(&mut buf).await;
    Ok(())
}

/// Derives the listen socket from a client `--addr` such as
/// `http://127.0.0.1:50051`. Only IP hosts can be forwarded to `metteurd`.
pub fn listen_from_addr(addr: &str) -> anyhow::Result<SocketAddr> {
    let rest = addr.split_once("://").map(|(_, r)| r).unwrap_or(addr);
    let host_port = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => (host_port, "50051"),
    };
    let host = host.trim_matches(['[', ']']);
    let host = if host.is_empty() { "127.0.0.1" } else { host };
    let address = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    address
        .parse()
        .with_context(|| {
            format!("cannot auto-start daemon: {address} is not an IP address (start it manually)")
        })
}

/// Returns the absolute path of an executable found on `PATH`, if any.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let search = env::var_os("PATH")?;
    for dir in env::split_paths(&search) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Reports whether a TCP connection to the address succeeds.
async fn tcp_reachable(addr: &SocketAddr) -> bool {
    match tokio::net::TcpStream::connect(addr).await {
        Ok(_) => true,
        Err(err) if err.kind() == io::ErrorKind::ConnectionRefused => false,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_config(binary: PathBuf, listen: &str) -> DaemonSpawn {
        DaemonSpawn {
            binary,
            listen_addr: listen.parse().unwrap(),
            data_dir: None,
            config: None,
            wake_path: None,
            pid_file: None,
            tls_cert: None,
            tls_key: None,
            tls_client_ca: None,
        }
    }

    #[test]
    fn listen_is_derived_from_client_addr() {
        assert_eq!(
            listen_from_addr("http://127.0.0.1:50051").unwrap(),
            "127.0.0.1:50051".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            listen_from_addr("https://[::1]:6000").unwrap(),
            "[::1]:6000".parse::<SocketAddr>().unwrap()
        );
        assert!(listen_from_addr("http://daemon.local:50051").is_err());
    }

    #[test]
    fn daemon_command_forwards_listen_and_data_dir() {
        let cfg = spawn_config(PathBuf::from("metteurd"), "127.0.0.1:50051");
        let mut cfg = cfg;
        cfg.data_dir = Some(PathBuf::from("/tmp/data"));
        cfg.config = Some(PathBuf::from("/tmp/met.yml"));
        cfg.wake_path = Some(PathBuf::from("/tmp/wake.sock"));
        cfg.pid_file = Some(PathBuf::from("/tmp/daemon.pid"));
        let cmd = build_daemon_command(&cfg);
        let args: Vec<_> = cmd.get_args().map(|s| s.to_string_lossy().into_owned()).collect();
        assert!(args.windows(2).any(|w| w == ["--listen-addr", "127.0.0.1:50051"]));
        assert!(args.windows(2).any(|w| w == ["--data-dir", "/tmp/data"]));
        assert!(args.windows(2).any(|w| w == ["--config", "/tmp/met.yml"]));
        assert!(args.windows(2).any(|w| w == ["--wake-path", "/tmp/wake.sock"]));
        assert!(args.windows(2).any(|w| w == ["--pid-file", "/tmp/daemon.pid"]));
        assert!(args.contains(&"--passive".to_string()));
        assert!(args.contains(&"--detach".to_string()));
    }

    #[test]
    fn daemon_command_sets_no_tls_without_all_paths() {
        let cfg = spawn_config(PathBuf::from("metteurd"), "127.0.0.1:50051");
        let cmd = build_daemon_command(&cfg);
        let args: Vec<_> = cmd.get_args().map(|s| s.to_string_lossy().into_owned()).collect();
        assert!(!args.contains(&"--tls-cert".to_string()));
    }

    #[test]
    fn locate_prefers_explicit_binary() {
        let dir = env::temp_dir().join(format!("metteur-spawn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = if cfg!(windows) { dir.join("metteurd.exe") } else { dir.join("metteurd") };
        std::fs::write(&binary, b"#!/bin/sh").unwrap();
        assert_eq!(locate_daemon(Some(&binary)).unwrap(), binary);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn explicit_missing_binary_is_an_error() {
        assert!(locate_daemon(Some(Path::new("definitely/not/here"))).is_err());
    }
}