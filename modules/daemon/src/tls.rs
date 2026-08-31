//! Optional mutual TLS configuration for the gRPC server.

use std::path::Path;

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tonic::transport::{Certificate, Identity, ServerTlsConfig};

use crate::error::{DaemonError, DaemonResult};

/// Installs the default rustls crypto provider, if not already installed.
///
/// rustls 0.23 no longer auto-selects a provider; calling this before TLS is
/// used avoids a runtime panic.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Server-side mTLS configuration loaded from PEM files.
#[derive(Clone)]
pub struct DaemonTlsConfig {
    /// The tonic TLS server config.
    pub server: ServerTlsConfig,
}

/// Loads an mTLS config from the server certificate, private key and client
/// CA. When configured, clients must present a certificate signed by `ca`.
pub fn load(cert_path: &Path, key_path: &Path, ca_path: &Path) -> DaemonResult<DaemonTlsConfig> {
    install_crypto_provider();
    let cert = std::fs::read_to_string(cert_path).map_err(DaemonError::Io)?;
    let key = std::fs::read_to_string(key_path).map_err(DaemonError::Io)?;
    let ca = std::fs::read_to_string(ca_path).map_err(DaemonError::Io)?;

    // Validate before handing to tonic, which panics on malformed PEM.
    if CertificateDer::from_pem_slice(cert.as_bytes())
        .map_err(|e| DaemonError::Tls(format!("invalid certificate: {e}")))?
        .is_empty()
    {
        return Err(DaemonError::Tls("certificate is empty".to_string()));
    }
    PrivateKeyDer::from_pem_slice(key.as_bytes())
        .map_err(|e| DaemonError::Tls(format!("invalid private key: {e}")))?;
    if CertificateDer::from_pem_slice(ca.as_bytes())
        .map_err(|e| DaemonError::Tls(format!("invalid client ca: {e}")))?
        .is_empty()
    {
        return Err(DaemonError::Tls("client ca is empty".to_string()));
    }

    Ok(DaemonTlsConfig {
        server: ServerTlsConfig::new()
            .identity(Identity::from_pem(cert, key))
            .client_ca_root(Certificate::from_pem(ca))
            .client_auth_optional(false),
    })
}
