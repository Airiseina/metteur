//! Addon manifest parsing and validation.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{DaemonError, DaemonResult};
use crate::registry::tool::is_valid_tool_name;

/// One tool exported by an addon.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolEntry {
    /// Registered name; must be PascalCase imperative.
    pub name: String,
    /// The wasm export invoked for calls.
    pub function: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema of the arguments, inlined as a TOML table.
    #[serde(default = "default_parameters")]
    pub parameters: toml::Table,
}

fn default_parameters() -> toml::Table {
    toml::Table::new()
}

/// One prompt fragment contributed by an addon.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentEntry {
    pub name: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub scope: String,
    /// Package-relative UTF-8 text file with the fragment content.
    pub file: String,
}

/// The parsed and validated `manifest.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Reverse-DNS identifier, e.g. `com.example.stats`.
    pub id: String,
    pub version: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// SemVer requirement matched against the daemon version.
    #[serde(default)]
    pub metteur_version: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub permissions: PermissionsSection,
    #[serde(default)]
    pub addon: AddonSection,
    #[serde(default)]
    pub tools: Vec<ToolEntry>,
    #[serde(default)]
    pub fragments: Vec<FragmentEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PermissionsSection {
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct AddonSection {
    /// Package-relative path of the wasm module.
    pub entry: String,
    /// Per-call timeout override in milliseconds (0 = global default).
    #[serde(default)]
    pub call_timeout_ms: u64,
}

impl Manifest {
    /// Parses and validates the manifest text located at `package_dir`.
    ///
    /// Validation covers required fields, id format, tool naming, entry
    /// existence, fragment file existence and the metteur version range.
    pub fn load(package_dir: &Path) -> DaemonResult<(Self, PathBuf)> {
        let manifest_path = package_dir.join("manifest.toml");
        let text = std::fs::read_to_string(&manifest_path).map_err(|err| {
            DaemonError::Addon(format!("cannot read {}: {err}", manifest_path.display()))
        })?;
        let manifest: Manifest =
            toml::from_str(&text).map_err(|err| DaemonError::Addon(err.to_string()))?;
        manifest.validate(package_dir)?;
        Ok((manifest, package_dir.to_path_buf()))
    }

    fn validate(&self, package_dir: &Path) -> DaemonResult<()> {
        if self.id.is_empty() || self.version.is_empty() || self.name.is_empty() {
            return Err(DaemonError::Addon("id, version and name are required".to_string()));
        }
        // Reverse-DNS id: lowercase dot-separated non-empty labels.
        let labels: Vec<&str> = self.id.split('.').collect();
        if labels.len() < 2
            || labels.iter().any(|label| {
                label.is_empty()
                    || !label.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
            })
        {
            return Err(DaemonError::Addon(format!(
                "addon id '{}' must be reverse-DNS (e.g. com.example.stats)",
                self.id
            )));
        }
        if semver::Version::parse(&self.version).is_err() {
            return Err(DaemonError::Addon(format!("invalid addon version '{}'", self.version)));
        }
        if !self.metteur_version.is_empty() {
            let requirement = semver::VersionReq::parse(&self.metteur_version)
                .map_err(|err| DaemonError::Addon(format!("invalid metteur_version: {err}")))?;
            let current =
                semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("workspace version");
            if !requirement.matches(&current) {
                return Err(DaemonError::Addon(format!(
                    "addon requires metteur {}, but daemon is {}",
                    self.metteur_version, current
                )));
            }
        }
        if self.addon.entry.is_empty() {
            return Err(DaemonError::Addon("[addon] entry is required".to_string()));
        }
        let entry_path = package_dir.join(&self.addon.entry);
        if !entry_path.is_file() {
            return Err(DaemonError::Addon(format!(
                "wasm entry not found: {}",
                entry_path.display()
            )));
        }
        for tool in &self.tools {
            if !is_valid_tool_name(&tool.name) {
                return Err(DaemonError::Addon(format!(
                    "tool name '{}' must be PascalCase imperative",
                    tool.name
                )));
            }
            if tool.function.is_empty() {
                return Err(DaemonError::Addon(format!("tool '{}' has no function", tool.name)));
            }
        }
        for fragment in &self.fragments {
            let path = package_dir.join(&fragment.file);
            if !path.is_file() {
                return Err(DaemonError::Addon(format!(
                    "fragment file not found: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    /// Resolved per-call timeout, falling back to the section default.
    pub fn call_timeout_ms(&self, fallback_ms: u64) -> u64 {
        if self.addon.call_timeout_ms != 0 {
            self.addon.call_timeout_ms
        } else if fallback_ms == 0 {
            DEFAULT_CALL_TIMEOUT_MS
        } else {
            fallback_ms
        }
    }

    /// Returns the required permissions not covered by `granted`.
    pub fn missing_permissions(&self, granted: &[String]) -> Vec<String> {
        self.permissions
            .required
            .iter()
            .filter(|required| !granted.contains(required))
            .cloned()
            .collect()
    }

    /// Reads all fragment files as `(fragment, content)` pairs.
    pub fn load_fragments(&self, package_dir: &Path) -> DaemonResult<Vec<(FragmentEntry, String)>> {
        self.fragments
            .iter()
            .map(|fragment| {
                let path = package_dir.join(&fragment.file);
                let content = std::fs::read_to_string(&path).map_err(|err| {
                    DaemonError::Addon(format!("cannot read {}: {err}", path.display()))
                })?;
                Ok((fragment.clone(), content))
            })
            .collect()
    }
}

/// Default plugin-call timeout when neither config nor manifest set one.
const DEFAULT_CALL_TIMEOUT_MS: u64 = 30_000;

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, body: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("manifest.toml");
        std::fs::write(&path, body).unwrap();
        dir.to_path_buf()
    }

    const VALID: &str = r#"
id = "com.example.stats"
version = "0.1.0"
name = "Stats Tools"
metteur_version = ">=0.0.1"

[permissions]
required = ["tools"]

[addon]
entry = "main.wasm"

[[tools]]
name = "WordCount"
function = "word_count"
description = "Counts words."
[tools.parameters]
type = "object"

[[fragments]]
name = "style_guide"
file = "style.md"
"#;

    #[test]
    fn accepts_valid_manifest_with_matching_version() {
        let dir = write_manifest(
            &std::env::temp_dir().join(format!("addon-ok-{}", uuid::Uuid::new_v4())),
            VALID,
        );
        std::fs::write(dir.join("main.wasm"), b"fake").unwrap();
        std::fs::write(dir.join("style.md"), "text").unwrap();
        let (manifest, _) = Manifest::load(&dir).unwrap();
        assert_eq!(manifest.tools.len(), 1);
        assert_eq!(manifest.missing_permissions(&["tools".into()]), Vec::<String>::new());
        assert_eq!(manifest.missing_permissions(&[]), vec!["tools".to_string()]);
    }

    #[test]
    fn rejects_bad_id_or_missing_entry_or_version_conflict() {
        let base = std::env::temp_dir().join(format!("addon-bad-{}", uuid::Uuid::new_v4()));

        let bad_id =
            write_manifest(&base.join("a"), &VALID.replace("com.example.stats", "Not An Id"));
        std::fs::write(base.join("a").join("main.wasm"), b"x").unwrap();
        std::fs::write(base.join("a").join("style.md"), "t").unwrap();
        assert!(Manifest::load(&bad_id).is_err());

        let no_entry = write_manifest(&base.join("b"), VALID);
        std::fs::write(base.join("b").join("style.md"), "t").unwrap();
        assert!(Manifest::load(&no_entry).is_err());
        std::fs::write(base.join("b").join("main.wasm"), b"x").unwrap();
        assert!(Manifest::load(&no_entry).is_ok());

        let future = write_manifest(&base.join("c"), &VALID.replace(">=0.0.1", ">=99.0.0"));
        std::fs::write(base.join("c").join("main.wasm"), b"x").unwrap();
        std::fs::write(base.join("c").join("style.md"), "t").unwrap();
        assert!(Manifest::load(&future).is_err());
    }

    #[test]
    fn rejects_non_pascal_tool_names() {
        let dir = std::env::temp_dir().join(format!("addon-name-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let body = VALID.replace("WordCount", "word_count");
        std::fs::write(dir.join("manifest.toml"), body).unwrap();
        std::fs::write(dir.join("main.wasm"), b"x").unwrap();
        std::fs::write(dir.join("style.md"), "t").unwrap();
        assert!(Manifest::load(&dir).is_err());
    }
}
