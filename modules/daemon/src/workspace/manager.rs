//! Workspace manager and active workspace registry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use metteur_shared::config::Config;
use tokio::sync::RwLock;

use super::lock::SessionLock;
use crate::config;
use crate::error::{DaemonError, DaemonResult};
use crate::persistence::Db;
use crate::versioning::{VersionManager, WorkspaceWatcher};

/// The name of the workspace metadata directory.
pub const METADATA_DIR: &str = ".metteur";

/// A single open workspace.
pub struct Workspace {
    /// Absolute path to the workspace root.
    pub root: PathBuf,
    /// Path to the `/.metteur` metadata directory.
    #[allow(dead_code)]
    pub metadata_dir: PathBuf,
    /// The merged (global + workspace) configuration.
    pub config: Arc<RwLock<Config>>,
    /// The workspace-local database.
    pub db: Db,
    /// The workspace version manager (snapshots, file history).
    pub version_manager: Arc<VersionManager>,
    /// Language-server manager, when LSP is enabled for this workspace.
    pub lsp_manager: Option<Arc<crate::lsp::LspManager>>,
    /// The held session lock.
    _lock: SessionLock,
    /// The fs watcher feeding auto snapshots and live change events.
    watcher: Option<WorkspaceWatcher>,
}

impl Workspace {
    /// Returns the workspace root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the live fs watcher, if one could be started.
    pub fn watcher(&self) -> Option<&WorkspaceWatcher> {
        self.watcher.as_ref()
    }
}

/// Manages the set of active workspaces.
#[derive(Default)]
pub struct WorkspaceManager {
    active: RwLock<HashMap<PathBuf, Arc<Workspace>>>,
    /// Global config file used when merging workspace configs (`--config`).
    global_config_path: Option<PathBuf>,
}

impl WorkspaceManager {
    /// Creates a new empty workspace manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the global config file used when merging workspace configs.
    pub fn with_global_config_path(mut self, path: PathBuf) -> Self {
        self.global_config_path = Some(path);
        self
    }

    /// Resolves the effective global config file path.
    pub fn global_config_path(&self) -> DaemonResult<PathBuf> {
        match &self.global_config_path {
            Some(path) => Ok(path.clone()),
            None => config::default_global_config_path(),
        }
    }

    /// Opens a workspace at `root`, creating its metadata directory if needed.
    ///
    /// If the directory is not already a workspace, the metadata directory is
    /// created (similar to `git init`). Returns an error if the workspace is
    /// already open or locked by another session.
    pub async fn open(&self, root: &Path) -> DaemonResult<Arc<Workspace>> {
        let root = normalize_path(&root.canonicalize().map_err(|e| {
            DaemonError::Io(std::io::Error::new(
                e.kind(),
                format!("cannot resolve workspace path {}: {e}", root.display()),
            ))
        })?);

        {
            let active = self.active.read().await;
            if let Some(ws) = active.get(&root) {
                return Ok(ws.clone());
            }
        }

        let metadata_dir = root.join(METADATA_DIR);
        let lock = SessionLock::acquire(&metadata_dir)?;
        let db = Db::open(&metadata_dir.join("db"))?;
        let global_config_path = self.global_config_path()?;
        let config = config::load_merged_config(&global_config_path, &root)?;
        let version_manager = Arc::new(VersionManager::new(db.clone(), root.clone()));
        let lsp_manager = crate::lsp::LspManager::new(&config.lsp, &root);
        let watcher = match WorkspaceWatcher::start(
            root.clone(),
            version_manager.clone(),
            config.versioning.auto_snapshot,
        ) {
            Ok(watcher) => Some(watcher),
            Err(err) => {
                tracing::warn!("failed to start file watcher: {err}");
                None
            }
        };

        let workspace = Arc::new(Workspace {
            root: root.clone(),
            metadata_dir,
            config: Arc::new(RwLock::new(config)),
            db,
            version_manager,
            lsp_manager,
            _lock: lock,
            watcher,
        });

        self.active.write().await.insert(root.clone(), workspace.clone());
        Ok(workspace)
    }

    /// Closes the workspace at `root`, releasing its lock.
    pub async fn close(&self, root: &Path) -> DaemonResult<()> {
        let root = normalize_path(&root.canonicalize().map_err(|e| {
            DaemonError::Io(std::io::Error::new(
                e.kind(),
                format!("cannot resolve workspace path {}: {e}", root.display()),
            ))
        })?);
        let removed = self.active.write().await.remove(&root);
        let Some(workspace) = removed else {
            return Err(DaemonError::NotFound(format!("workspace {} is not open", root.display())));
        };
        // Stop language servers before the session lock is released.
        if let Some(lsp) = &workspace.lsp_manager {
            lsp.shutdown().await;
        }
        Ok(())
    }

    /// Returns the open workspace at `root`, if any.
    pub async fn get(&self, root: &Path) -> Option<Arc<Workspace>> {
        let root = normalize_path(&root.canonicalize().ok()?);
        self.active.read().await.get(&root).cloned()
    }

    /// Returns all open workspaces.
    pub async fn list(&self) -> Vec<Arc<Workspace>> {
        self.active.read().await.values().cloned().collect()
    }
}

/// Converts a path to a normalized `PathBuf`.
///
/// On Windows, strips the `\\?\` extended-length prefix that `canonicalize`
/// may add, so that workspace paths compare and display consistently.
fn normalize_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = path.to_string_lossy();
        let stripped = s.strip_prefix(r"\\?\").unwrap_or(&s);
        PathBuf::from(stripped.to_string())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opens_and_closes_workspace() {
        let dir = std::env::temp_dir().join(format!("metteur-ws-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let manager = WorkspaceManager::new();
        let _ws = manager.open(&dir).await.unwrap();
        assert!(dir.join(METADATA_DIR).exists());
        assert!(manager.get(&dir).await.is_some());
        manager.close(&dir).await.unwrap();
        assert!(manager.get(&dir).await.is_none());
    }

    #[tokio::test]
    async fn open_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("metteur-ws-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let manager = WorkspaceManager::new();
        let a = manager.open(&dir).await.unwrap();
        let b = manager.open(&dir).await.unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
