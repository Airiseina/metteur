//! Offline addon signing tool.
//!
//! `metteurd --gen-signing-key <path>` creates a fresh Ed25519 keypair and
//! prints the base64 public key (to paste into `[addon] signing_keys`).
//! `metteurd --sign-addon <dir> --key-file <path>` writes a `signature.toml`
//! into an unpacked addon directory, making it installable on a daemon that
//! requires signatures.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use rand_core::OsRng;

use crate::cli::Cli;
use crate::error::{DaemonError, DaemonResult};

/// Entry point for the signing flags; exits after the requested operation.
pub fn run(cli: &Cli) -> DaemonResult<()> {
    if let Some(seed_path) = &cli.gen_signing_key {
        let public_key = generate_key(seed_path)?;
        println!("{public_key}");
        return Ok(());
    }

    let key = load_key(cli.key_file.as_deref())?;
    let package_dir = cli
        .sign_addon
        .as_deref()
        .ok_or_else(|| DaemonError::Addon("--sign-addon <dir> is required".to_string()))?;
    let content = crate::addon::signature::sign_package(package_dir, &key)?;
    std::fs::write(package_dir.join("signature.toml"), content).map_err(DaemonError::Io)?;
    println!("wrote {}", package_dir.join("signature.toml").display());
    Ok(())
}

/// Generates a fresh keypair, stores the base64 seed in `path` and returns the
/// base64 public key for the daemon configuration.
fn generate_key(path: &Path) -> DaemonResult<String> {
    let signing_key = SigningKey::generate(&mut OsRng);
    std::fs::write(path, B64.encode(signing_key.to_bytes())).map_err(DaemonError::Io)?;
    Ok(B64.encode(signing_key.verifying_key().to_bytes()))
}

/// Loads a base64-encoded 32-byte Ed25519 seed from `path`.
fn load_key(path: Option<&Path>) -> DaemonResult<SigningKey> {
    let path =
        path.ok_or_else(|| DaemonError::Addon("--key-file <path> is required".to_string()))?;
    let text = std::fs::read_to_string(path)
        .map_err(|err| DaemonError::NotFound(format!("cannot read key file: {err}")))?;
    let bytes = B64
        .decode(text.trim())
        .map_err(|_| DaemonError::Addon("key seed is not valid base64".to_string()))?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| DaemonError::Addon("key seed must encode exactly 32 bytes".to_string()))?;
    Ok(SigningKey::from_bytes(&seed))
}
