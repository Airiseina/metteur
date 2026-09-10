//! mTLS certificate issuance for a self-hosted daemon.
//!
//! `metteurd --gen-certs` creates a private CA, a server certificate carrying
//! the reachable listen addresses as SANs, and a client certificate identified
//! by its Common Name (used by the ACL rules). Every file is PEM-encoded and
//! written into a single TLS directory.

use std::net::IpAddr;
use std::path::PathBuf;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, SanType};

use crate::cli::Cli;
use crate::error::{DaemonError, DaemonResult};

/// File names written by [`generate`].
pub const CA_CERT: &str = "ca.pem";
/// Server certificate file name.
pub const SERVER_CERT: &str = "server.pem";
/// Server private key file name.
pub const SERVER_KEY: &str = "server.key";
/// Client certificate file name.
pub const CLIENT_CERT: &str = "client.pem";
/// Client private key file name.
pub const CLIENT_KEY: &str = "client.key";

/// Default Common Name for the generated client certificate.
pub const DEFAULT_CLIENT_CN: &str = "metteur-cli";

/// Options controlling [`generate`].
pub struct CertOptions {
    /// Directory the PEM files are written to; created when missing.
    pub dir: PathBuf,
    /// CA certificate Common Name.
    pub ca_cn: String,
    /// Server certificate Common Name.
    pub server_cn: String,
    /// Extra server SANs (DNS names or IP addresses) besides `localhost` and
    /// `127.0.0.1`.
    pub server_sans: Vec<String>,
    /// Client certificate Common Name.
    pub client_cn: String,
}

impl Default for CertOptions {
    fn default() -> Self {
        Self {
            dir: PathBuf::new(),
            ca_cn: "metteur ca".to_string(),
            server_cn: "localhost".to_string(),
            server_sans: Vec::new(),
            client_cn: DEFAULT_CLIENT_CN.to_string(),
        }
    }
}

/// Entry point for `metteurd --gen-certs`.
///
/// Resolves the output directory (a `--tls-dir` override or the global config
/// directory's `tls` subfolder) and issues the certificate set.
pub fn run(cli: &Cli) -> DaemonResult<()> {
    let dir = match &cli.tls_dir {
        Some(dir) => dir.clone(),
        None => crate::config::global_config_dir()?.join("tls"),
    };
    generate(&CertOptions {
        dir,
        server_sans: cli.server_san.clone(),
        client_cn: cli.cert_cn.clone(),
        ..Default::default()
    })
}

/// Issues a self-signed CA plus a server and a client certificate into `opts.dir`.
///
/// The server certificate always carries `localhost` and `127.0.0.1` as SANs so
/// that the default loopback gRPC address verifies; additional SANs may be
/// supplied in `opts.server_sans`. Existing files are overwritten.
pub fn generate(opts: &CertOptions) -> DaemonResult<()> {
    std::fs::create_dir_all(&opts.dir).map_err(DaemonError::Io)?;

    // Self-signed CA.
    let ca_key = KeyPair::generate().map_err(rcgen_err)?;
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).map_err(rcgen_err)?;
    ca_params.distinguished_name.push(DnType::CommonName, opts.ca_cn.clone());
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).map_err(rcgen_err)?;
    let issuer = Issuer::new(ca_params, ca_key);

    // Server certificate signed by the CA. DNS names and IP addresses are
    // passed separately: rcgen seeds the SAN list from the DNS names in `new`.
    let server_key = KeyPair::generate().map_err(rcgen_err)?;
    let mut server_params = CertificateParams::new(server_dns_names(opts)).map_err(rcgen_err)?;
    server_params.distinguished_name.push(DnType::CommonName, opts.server_cn.clone());
    server_params.subject_alt_names.push(SanType::IpAddress("127.0.0.1".parse().map_err(ip_err)?));
    for san in &opts.server_sans {
        if let Ok(ip) = san.parse::<IpAddr>() {
            server_params.subject_alt_names.push(SanType::IpAddress(ip));
        }
    }
    let server_cert = server_params.signed_by(&server_key, &issuer).map_err(rcgen_err)?;

    // Client certificate signed by the CA, identified by CN.
    let client_key = KeyPair::generate().map_err(rcgen_err)?;
    let mut client_params = CertificateParams::new(Vec::<String>::new()).map_err(rcgen_err)?;
    client_params.distinguished_name.push(DnType::CommonName, opts.client_cn.clone());
    let client_cert = client_params.signed_by(&client_key, &issuer).map_err(rcgen_err)?;

    write(&opts.dir.join(CA_CERT), ca_cert.pem().as_bytes())?;
    write(&opts.dir.join(SERVER_CERT), server_cert.pem().as_bytes())?;
    write(&opts.dir.join(SERVER_KEY), server_key.serialize_pem().as_bytes())?;
    write(&opts.dir.join(CLIENT_CERT), client_cert.pem().as_bytes())?;
    write(&opts.dir.join(CLIENT_KEY), client_key.serialize_pem().as_bytes())?;

    tracing::info!(
        "wrote certificates to {} (server CN={}, client CN={})",
        opts.dir.display(),
        opts.server_cn,
        opts.client_cn
    );
    Ok(())
}

/// Maps an rcgen error onto the daemon error type.
fn rcgen_err(e: rcgen::Error) -> DaemonError {
    DaemonError::Tls(e.to_string())
}

/// Writes `bytes` to `path`.
fn write(path: &PathBuf, bytes: &[u8]) -> DaemonResult<()> {
    std::fs::write(path, bytes).map_err(DaemonError::Io)
}

/// Builds the DNS-name SAN list for the server certificate: always `localhost`,
/// then every `server_san` entry that is not an IP literal.
fn server_dns_names(opts: &CertOptions) -> Vec<String> {
    let mut dns = vec!["localhost".to_string()];
    for san in &opts.server_sans {
        if san.parse::<IpAddr>().is_err() {
            dns.push(san.clone());
        }
    }
    dns
}

/// Builds the error used when a loopback IP literal fails to parse.
fn ip_err(e: std::net::AddrParseError) -> DaemonError {
    DaemonError::Tls(format!("invalid IP address: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("metteur-cert-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn generates_three_certificates() {
        let dir = temp_dir("gen");
        let opts = CertOptions {
            dir,
            server_sans: vec!["daemon.local".to_string(), "192.168.1.10".to_string()],
            ..Default::default()
        };
        generate(&opts).unwrap();
        for name in [CA_CERT, SERVER_CERT, SERVER_KEY, CLIENT_CERT, CLIENT_KEY] {
            assert!(opts.dir.join(name).exists(), "missing {name}");
        }
    }

    #[test]
    fn certs_are_pem_encoded() {
        let dir = temp_dir("pem");
        generate(&CertOptions {
            dir: dir.clone(),
            ..Default::default()
        })
        .unwrap();
        let cert = std::fs::read_to_string(dir.join(SERVER_CERT)).unwrap();
        let key = std::fs::read_to_string(dir.join(SERVER_KEY)).unwrap();
        assert!(cert.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(key.starts_with("-----BEGIN PRIVATE KEY-----"));
    }

    #[test]
    fn splits_dns_and_ip_sans() {
        let opts = CertOptions {
            server_sans: vec!["ha.local".to_string(), "192.168.1.10".to_string()],
            ..Default::default()
        };
        let dns = server_dns_names(&opts);
        assert!(dns.contains(&"localhost".to_string()));
        assert!(dns.contains(&"ha.local".to_string()));
        assert!(!dns.iter().any(|n| n == &"192.168.1.10".to_string()));
    }
}
