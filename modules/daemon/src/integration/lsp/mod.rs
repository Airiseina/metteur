//! LSP client stack: JSON-RPC framing, per-language clients and the manager.

pub mod client;
pub mod debounce;
pub mod rpc;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use metteur_shared::config::LspConfig;
use tokio::sync::Mutex;

use crate::error::{DaemonError, DaemonResult};

use client::LspClient;

/// Per-workspace manager of language server processes.
///
/// Clients are started lazily on the first request touching a file whose
/// extension maps to a configured language; all servers are shut down when
/// the workspace closes.
pub struct LspManager {
    workspace_root: PathBuf,
    languages: Vec<metteur_shared::config::LspLanguageConfig>,
    clients: Mutex<HashMap<String, ClientHandle>>,
    /// Snapshot of the config this manager was built from, used to decide
    /// whether a config change requires rebuilding it.
    config: LspConfig,
    /// Per-document sync cooldown merging rapid re-syncs.
    cooldown: tokio::sync::Mutex<crate::integration::lsp::debounce::SyncCooldown>,
}

/// Returns whether a live manager no longer matches `config`.
///
/// Used to skip needless restarts: a config write that does not touch LSP
/// settings must not tear down running language servers.
pub fn config_changed(config: &LspConfig, current: Option<&LspManager>) -> bool {
    match current {
        Some(manager) => manager.config != *config,
        // No live manager: only a config that would produce one matters.
        None => config.enabled && !config.languages.is_empty(),
    }
}

struct ClientHandle {
    client: Arc<LspClient>,
    child: tokio::process::Child,
}

impl LspManager {
    /// Creates a manager when LSP is enabled and at least one language is
    /// configured; otherwise returns `None`.
    pub fn new(config: &LspConfig, workspace_root: &Path) -> Option<Arc<Self>> {
        if !config.enabled || config.languages.is_empty() {
            return None;
        }
        Some(Arc::new(Self {
            workspace_root: workspace_root.to_path_buf(),
            languages: config.languages.clone(),
            clients: Mutex::new(HashMap::new()),
            config: config.clone(),
            cooldown: tokio::sync::Mutex::new(
                crate::integration::lsp::debounce::SyncCooldown::new(
                    std::time::Duration::from_millis(config.debounce_ms),
                ),
            ),
        }))
    }

    /// Returns the language id mapped to the given file extension.
    pub fn language_for_extension(&self, extension: &str) -> Option<String> {
        self.languages
            .iter()
            .find(|language| {
                language
                    .extensions
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(extension))
            })
            .map(|language| language.id.clone())
    }

    /// Returns the workspace root this manager is bound to.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Returns the effective document sync merge window.
    pub fn debounce(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.config.debounce_ms)
    }

    /// Returns whether a diagnostics pass should run when a node mutates files.
    pub fn check_on_node_end(&self) -> bool {
        self.config.check_on_node_end
    }

    /// Decides whether a document sync should run now.
    ///
    /// `explicit` callers (an LLM tool invocation or a validation node) always
    /// sync, because observing the current state is their entire purpose.
    /// Implicit callers within the merge window are told to reuse the most
    /// recent diagnostics instead of pushing an intermediate revision.
    pub async fn should_sync(
        &self,
        uri: &str,
        explicit: bool,
    ) -> crate::integration::lsp::debounce::SyncDecision {
        let now = std::time::Instant::now();
        let mut cooldown = self.cooldown.lock().await;
        cooldown.decide(uri, now, explicit)
    }

    /// Forgets the cooldown state of a document (its file disappeared).
    pub async fn forget_document(&self, uri: &str) {
        self.cooldown.lock().await.forget(uri);
    }

    /// Returns (starting it if needed) the client for a file extension.
    pub async fn client_for_extension(
        &self,
        extension: &str,
    ) -> DaemonResult<Option<Arc<LspClient>>> {
        let Some(language_id) = self.language_for_extension(extension) else {
            return Ok(None);
        };
        let mut clients = self.clients.lock().await;
        if let Some(handle) = clients.get(&language_id) {
            return Ok(Some(handle.client.clone()));
        }
        let definition = self
            .languages
            .iter()
            .find(|language| language.id == language_id)
            .ok_or_else(|| DaemonError::Lsp(format!("language {language_id} vanished")))?;
        let (program, args) = definition.command.split_first().ok_or_else(|| {
            DaemonError::Lsp(format!("language {language_id} has an empty command"))
        })?;

        let mut child = tokio::process::Command::new(program)
            .args(args)
            .current_dir(&self.workspace_root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|err| {
                DaemonError::Lsp(format!("failed to start {language_id} server: {err}"))
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| DaemonError::Lsp("server stdin unavailable".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DaemonError::Lsp("server stdout unavailable".to_string()))?;

        let client = LspClient::over_streams(Box::new(stdout), Box::new(stdin));
        if let Err(err) = client.initialize(&self.workspace_root).await {
            // Never leak the spawned server process on a failed handshake.
            let _ = child.kill().await;
            return Err(err);
        }
        clients.insert(
            language_id,
            ClientHandle {
                client: client.clone(),
                child,
            },
        );
        Ok(Some(client))
    }

    /// Shuts down every running language server.
    pub async fn shutdown(&self) {
        let mut clients = self.clients.lock().await;
        for (_, handle) in clients.drain() {
            handle.client.shutdown().await;
            let mut child = handle.child;
            let _ = child.kill().await;
        }
    }

    /// Converts a workspace-relative path into a `file://` URI.
    pub fn to_file_uri(&self, absolute: &Path) -> String {
        metteur_shared::Uri::from_path(absolute).to_string()
    }
}
