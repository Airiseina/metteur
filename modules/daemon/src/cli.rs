//! Command-line argument parsing for the daemon.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// The Metteur daemon.
#[derive(Debug, Parser)]
#[command(name = "metteurd", version, about)]
pub struct Cli {
    /// Path to an explicit config file.
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Directory for global data storage.
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Address to listen on for gRPC.
    #[arg(long, default_value = "127.0.0.1:50051")]
    pub listen_addr: SocketAddr,

    /// Detach from the controlling terminal and run as a background daemon.
    ///
    /// Keeps `--pid-file` (or the default under the global config dir) with
    /// the detached child's PID, then the parent process exits.
    #[arg(long)]
    pub detach: bool,

    /// Write the daemon PID here when `--detach` re-executes it.
    #[arg(long)]
    pub pid_file: Option<PathBuf>,

    /// Address serving Prometheus metrics at /metrics (disabled by default).
    #[arg(long)]
    pub metrics_addr: Option<SocketAddr>,

    /// Start in passive (wait-for-wake) mode.
    #[arg(long)]
    pub passive: bool,

    /// Path to the wake socket/pipe used in passive mode.
    #[arg(long)]
    pub wake_path: Option<PathBuf>,

    /// Register the daemon as a Windows service, then exit.
    #[arg(long)]
    pub install_service: bool,

    /// Unregister the Windows service, then exit.
    #[arg(long)]
    pub uninstall_service: bool,

    /// Run as a Windows service under the Service Control Manager.
    #[arg(long)]
    pub service: bool,

    /// Start the installed Windows service, then exit.
    #[arg(long)]
    pub start_service: bool,

    /// Stop the installed Windows service, then exit.
    #[arg(long)]
    pub stop_service: bool,

    /// Print the installed Windows service status, then exit.
    #[arg(long)]
    pub service_status: bool,

    /// Server TLS certificate (PEM). Requires `--tls-key` and
    /// `--tls-client-ca` to enable mTLS.
    #[arg(long)]
    pub tls_cert: Option<PathBuf>,

    /// Server TLS private key (PEM).
    #[arg(long)]
    pub tls_key: Option<PathBuf>,

    /// CA used to verify client certificates (PEM).
    #[arg(long)]
    pub tls_client_ca: Option<PathBuf>,

    /// Generate a CA, a server and a client certificate for mTLS, then exit.
    #[arg(long)]
    pub gen_certs: bool,

    /// Directory that generated certificates are written to. Defaults to the
    /// global config directory's `tls` subfolder.
    #[arg(long)]
    pub tls_dir: Option<PathBuf>,

    /// Common Name of the generated client certificate (used by ACL rules).
    #[arg(long, default_value = crate::cert::DEFAULT_CLIENT_CN)]
    pub cert_cn: String,

    /// Extra server Subject Alternative Names (DNS names or IP addresses).
    #[arg(long)]
    pub server_san: Vec<String>,

    /// Generate a fresh Ed25519 signing key, write the base64 seed to `path`
    /// and print the base64 public key, then exit.
    #[arg(long)]
    pub gen_signing_key: Option<PathBuf>,

    /// Sign the unpacked addon directory, writing its `signature.toml`.
    #[arg(long)]
    pub sign_addon: Option<PathBuf>,

    /// Base64-encoded Ed25519 seed used by `--sign-addon`.
    #[arg(long)]
    pub key_file: Option<PathBuf>,
}
