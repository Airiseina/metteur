//! Configuration loading and merging for the daemon.

use std::path::{Path, PathBuf};

use metteur_shared::config::Config;

use crate::error::{DaemonError, DaemonResult};

/// The name of the global config directory.
pub const CONFIG_DIR: &str = ".metteur";
/// The name of the global config file.
pub const CONFIG_FILE: &str = "config.toml";

/// Resolves the global config directory (e.g. `~/.metteur`).
pub fn global_config_dir() -> DaemonResult<PathBuf> {
    let dir = directories::BaseDirs::new()
        .ok_or_else(|| DaemonError::Internal("cannot resolve home directory".to_string()))?
        .home_dir()
        .join(CONFIG_DIR);
    Ok(dir)
}

/// Resolves the default global config file path.
pub fn default_global_config_path() -> DaemonResult<PathBuf> {
    Ok(global_config_dir()?.join(CONFIG_FILE))
}

/// Loads the global config from `path`, returning a default config if absent.
pub fn load_global_config(path: &Path) -> DaemonResult<Config> {
    load_config_file(path)
}

/// Loads the workspace config for the given workspace root.
///
/// Returns a default config if the workspace has no config file.
pub fn load_workspace_config(workspace_root: &Path) -> DaemonResult<Config> {
    let path = workspace_root.join(CONFIG_DIR).join(CONFIG_FILE);
    load_config_file(&path)
}

/// Loads a config from a file, returning a default config if absent.
fn load_config_file(path: &Path) -> DaemonResult<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let content = std::fs::read_to_string(path)?;
    let config = toml::from_str(&content).map_err(|e| {
        DaemonError::Serialization(format!("invalid config {}: {e}", path.display()))
    })?;
    Ok(config)
}

/// Loads and merges the global and workspace configs.
pub fn load_merged_config(global_path: &Path, workspace_root: &Path) -> DaemonResult<Config> {
    let global = load_global_config(global_path)?;
    let workspace = load_workspace_config(workspace_root)?;
    Ok(global.merge(&workspace))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_returns_default() {
        let dir = std::env::temp_dir().join(format!("metteur-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = load_config_file(&dir.join(CONFIG_FILE)).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn loads_toml_config() {
        let dir = std::env::temp_dir().join(format!("metteur-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CONFIG_FILE);
        std::fs::write(&path, "[llm]\ndefault_model = \"gpt-4\"\n").unwrap();
        let cfg = load_config_file(&path).unwrap();
        assert_eq!(cfg.llm.default_model.as_deref(), Some("gpt-4"));
    }

    #[test]
    fn merged_config_prefers_explicit_global_path() {
        let dir = std::env::temp_dir().join(format!("metteur-cfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let global = dir.join("global.toml");
        std::fs::write(&global, "[llm]\ndefault_model = \"override\"\n").unwrap();
        let ws_root = dir.join("ws");
        std::fs::create_dir_all(ws_root.join(CONFIG_DIR)).unwrap();
        std::fs::write(
            ws_root.join(CONFIG_DIR).join(CONFIG_FILE),
            "[versioning]\nauto_snapshot = true\n",
        )
        .unwrap();

        let merged = load_merged_config(&global, &ws_root).unwrap();
        assert_eq!(merged.llm.default_model.as_deref(), Some("override"));
        assert!(merged.versioning.auto_snapshot);
    }
}
