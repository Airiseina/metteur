//! Addon host: package lifecycle, tool registration and prompt fragments.

pub mod manifest;
pub mod runtime;
pub mod signature;
pub mod signer;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use metteur_shared::Value;
use parking_lot::RwLock;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::{DaemonError, DaemonResult};
use crate::registry::Tool;

use manifest::Manifest;
use runtime::Permission;

/// A loaded addon and its registration state.
struct Loaded {
    /// Registry names of the tools contributed by this addon.
    tool_names: Vec<String>,
}

/// Snapshot describing one installed addon (for RPC responses).
#[derive(Debug, Clone)]
pub struct AddonInfoData {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub scope: String,
    pub required_permissions: Vec<String>,
    pub granted_permissions: Vec<String>,
    pub tool_count: u32,
    pub fragment_count: u32,
}

/// Manages installed addons in the global directory.
///
/// Tools are registered under `{AddonIdPascal}{ToolNamePascal}` names; prompt
/// fragments are collected per workspace on demand.
pub struct AddonHost {
    global_dir: PathBuf,
    registry: Arc<crate::registry::Registry>,
    fallback_timeout_ms: u64,
    signature: signature::Policy,
    loaded: RwLock<BTreeMap<String, Loaded>>,
    /// Registry names of all addon tools, mirrored for the runtime's
    /// `call_tool` hidden list.
    addon_tool_names: Arc<RwLock<HashSet<String>>>,
    /// Serializes installs/uninstalls/rescans.
    maintenance: AsyncMutex<()>,
}

impl AddonHost {
    /// Creates a host rooted at `<data_dir>/addons`.
    pub fn new(
        data_dir: &Path,
        registry: Arc<crate::registry::Registry>,
        fallback_timeout_ms: u64,
        addon_cfg: &metteur_shared::config::AddonConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            global_dir: data_dir.join("addons"),
            registry,
            fallback_timeout_ms,
            signature: signature::Policy::from_config(addon_cfg),
            loaded: RwLock::new(BTreeMap::new()),
            addon_tool_names: Arc::new(RwLock::new(HashSet::new())),
            maintenance: AsyncMutex::new(()),
        })
    }

    fn workspace_addon_dir(workspace_root: &Path) -> PathBuf {
        workspace_root.join(crate::workspace::METADATA_DIR).join("addons")
    }

    fn disabled_marker(dir: &Path) -> PathBuf {
        dir.join("disabled")
    }

    /// Rescans the global addon directory, replacing all registrations.
    pub async fn rescan(&self) {
        let _guard = self.maintenance.lock().await;
        self.rescan_locked();
    }

    fn rescan_locked(&self) {
        let old_entries = std::mem::take(&mut *self.loaded.write());
        for (_, old) in old_entries {
            for name in &old.tool_names {
                self.registry.unregister_tool(name);
            }
        }
        let mut loaded = self.loaded.write();
        for dir in list_addon_dirs(&self.global_dir) {
            let Ok((manifest, package_dir)) = Manifest::load(&dir) else {
                tracing::warn!("skipping invalid addon package at {}", dir.display());
                continue;
            };
            if loaded.contains_key(&manifest.id) {
                // A previous directory already claimed this id; registering
                // both would leak the first set of tools on overwrite.
                tracing::warn!(
                    "duplicate addon id '{}' at {}, skipping",
                    manifest.id,
                    dir.display()
                );
                continue;
            }
            let mut names = Vec::with_capacity(manifest.tools.len());
            for tool_entry in &manifest.tools {
                let registered = format!("{}{}", pascal(&manifest.id), tool_entry.name);
                if let Err(err) = self.registry.try_register_tool(Arc::new(AddonTool {
                    package_dir: package_dir.clone(),
                    manifest: manifest.clone(),
                    function: tool_entry.function.clone(),
                    registered_name: registered.clone(),
                    description: tool_entry.description.clone(),
                    parameters_schema: toml_table_to_json(&tool_entry.parameters),
                    permissions: manifest
                        .permissions
                        .required
                        .iter()
                        .filter_map(|raw| Permission::parse(raw))
                        .collect(),
                    addon_tool_names: self.addon_tool_names.clone(),
                    host: self.fallback_timeout_ms,
                })) {
                    tracing::warn!(
                        "skipping addon tool '{registered}' of '{}': {err}",
                        manifest.id
                    );
                    continue;
                }
                names.push(registered);
            }
            tracing::info!(
                "addon '{}' v{} loaded with {} tool(s)",
                manifest.id,
                manifest.version,
                names.len()
            );
            loaded.insert(
                manifest.id.clone(),
                Loaded {
                    tool_names: names,
                },
            );
        }
        // Mirror every registered addon tool name for the runtime's hidden
        // list so addons cannot call each other.
        let all_names: HashSet<String> =
            loaded.values().flat_map(|entry| entry.tool_names.iter().cloned()).collect();
        *self.addon_tool_names.write() = all_names;
    }

    /// Installs a `.zip` package or unpacked directory.
    ///
    /// `workspace_path` selects the scope (`None` = global). Returns the
    /// resulting addon snapshot after a rescan.
    pub async fn install(
        &self,
        package_path: &Path,
        workspace_path: Option<&Path>,
        granted: &[String],
    ) -> DaemonResult<AddonInfoData> {
        let target_dir = match workspace_path {
            Some(root) => Self::workspace_addon_dir(root),
            None => self.global_dir.clone(),
        };

        // Validate before touching the destination; every package lives in
        // its own `<name>/` subdirectory under the scope root.
        let dest = if package_path.is_file() && has_zip_extension(package_path) {
            let stem = package_path
                .file_stem()
                .ok_or_else(|| DaemonError::Addon("invalid package name".to_string()))?;
            let dest = target_dir.join(stem);
            unzip_into(package_path, &dest)?;
            dest
        } else if package_path.is_dir() {
            let dest = target_dir.join(
                package_path
                    .file_name()
                    .ok_or_else(|| DaemonError::Addon("invalid package directory".to_string()))?,
            );
            if dest.exists() {
                std::fs::remove_dir_all(&dest).map_err(DaemonError::Io)?;
            }
            copy_tree(package_path, &dest)?;
            dest
        } else {
            return Err(DaemonError::NotFound(format!(
                "package not found: {}",
                package_path.display()
            )));
        };

        // Verify integrity before trusting the package contents. Any failure
        // here must not leave a broken package behind.
        let manifest = match self.finalize_install(&dest, granted) {
            Ok(manifest) => manifest,
            Err(err) => {
                let _ = std::fs::remove_dir_all(&dest);
                return Err(err);
            }
        };

        self.rescan().await;
        self.info(&manifest.id, workspace_path)?
            .ok_or_else(|| DaemonError::Addon("addon disappeared after installation".to_string()))
    }

    /// Validates the extracted package and persists the granted permissions.
    fn finalize_install(&self, dest: &Path, granted: &[String]) -> DaemonResult<Manifest> {
        // Reject tampered or unsigned packages first, per the policy.
        signature::verify(dest, &self.signature.trusted_keys, self.signature.require_signature)?;
        let (manifest, _) = Manifest::load(dest)?;
        let missing = manifest.missing_permissions(granted);
        if !missing.is_empty() {
            return Err(DaemonError::PermissionDenied(format!(
                "missing required permissions: {}",
                missing.join(", ")
            )));
        }
        for raw in granted {
            if Permission::parse(raw).is_none() {
                return Err(DaemonError::Addon(format!("unknown permission '{raw}'")));
            }
        }
        // Persist granted permissions so future rescans can report them.
        std::fs::write(dest.join("granted.toml"), toml_string(granted)).map_err(DaemonError::Io)?;
        Ok(manifest)
    }

    /// Removes an installed addon by id.
    pub async fn uninstall(&self, id: &str, workspace_path: Option<&Path>) -> DaemonResult<()> {
        let _guard = self.maintenance.lock().await;
        let dir = self.find_dir(id, workspace_path)?;
        std::fs::remove_dir_all(&dir).map_err(|err| {
            DaemonError::Addon(format!("failed to remove {}: {err}", dir.display()))
        })?;
        drop(_guard);
        self.rescan().await;
        Ok(())
    }

    /// Enables or disables an addon via its marker file.
    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
        workspace_path: Option<&Path>,
    ) -> DaemonResult<()> {
        let _guard = self.maintenance.lock().await;
        let dir = self.find_dir(id, workspace_path)?;
        let marker = Self::disabled_marker(&dir);
        if enabled {
            let _ = std::fs::remove_file(&marker);
        } else {
            std::fs::write(&marker, b"").map_err(DaemonError::Io)?;
        }
        drop(_guard);
        self.rescan().await;
        Ok(())
    }

    /// Lists installed addons across scopes.
    pub async fn list(&self, workspace_roots: &[PathBuf]) -> Vec<AddonInfoData> {
        let mut roots: Vec<Option<PathBuf>> = vec![None];
        roots.extend(workspace_roots.iter().map(|root| Some(root.clone())));
        let mut out = Vec::new();
        for scope in roots {
            let base = match &scope {
                None => self.global_dir.clone(),
                Some(root) => Self::workspace_addon_dir(root),
            };
            for dir in list_addon_dirs(&base) {
                if let Ok((manifest, _)) = Manifest::load(&dir) {
                    out.push(self.build_info(&dir, manifest, scope.is_none()));
                }
            }
        }
        out
    }

    fn find_dir(&self, id: &str, workspace_path: Option<&Path>) -> DaemonResult<PathBuf> {
        let base = match workspace_path {
            Some(root) => Self::workspace_addon_dir(root),
            None => self.global_dir.clone(),
        };
        match_addon_dir(&base, id)
            .ok_or_else(|| DaemonError::NotFound(format!("addon '{id}' is not installed")))
    }

    fn info(&self, id: &str, workspace_path: Option<&Path>) -> DaemonResult<Option<AddonInfoData>> {
        let base = match workspace_path {
            Some(root) => Self::workspace_addon_dir(root),
            None => self.global_dir.clone(),
        };
        Ok(match_addon_dir(&base, id).and_then(|dir| {
            // The package may have been corrupted after installation; report
            // nothing instead of failing the whole listing.
            Manifest::load(&dir)
                .ok()
                .map(|(manifest, _)| self.build_info(&dir, manifest, workspace_path.is_none()))
        }))
    }

    fn build_info(&self, dir: &Path, manifest: Manifest, global: bool) -> AddonInfoData {
        let granted = read_granted(dir);
        AddonInfoData {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            name: manifest.name.clone(),
            description: manifest.description.clone(),
            enabled: !Self::disabled_marker(dir).exists(),
            scope: if global {
                "global".to_string()
            } else {
                "workspace".to_string()
            },
            required_permissions: manifest.permissions.required.clone(),
            granted_permissions: granted,
            tool_count: manifest.tools.len() as u32,
            fragment_count: manifest.fragments.len() as u32,
        }
    }

    /// Collects prompt fragments from enabled addons for one workspace.
    pub async fn fragments_for(
        &self,
        workspace_root: &Path,
    ) -> Vec<metteur_shared::SystemFragment> {
        let _guard = self.maintenance.lock().await;
        let mut fragments = Vec::new();
        for base in [self.global_dir.clone(), Self::workspace_addon_dir(workspace_root)] {
            for dir in list_addon_dirs(&base) {
                if Self::disabled_marker(&dir).exists() {
                    continue;
                }
                let Ok((manifest, package_dir)) = Manifest::load(&dir) else {
                    continue;
                };
                match manifest.load_fragments(&package_dir) {
                    Ok(items) => {
                        for (entry, content) in items {
                            fragments.push(metteur_shared::SystemFragment {
                                priority: entry.priority,
                                scope: entry.scope,
                                content,
                            });
                        }
                    }
                    Err(err) => {
                        tracing::warn!("fragment load failed for '{}': {}", manifest.id, err)
                    }
                }
            }
        }
        fragments
    }
}

/// Registry adapter exposing one addon tool.
struct AddonTool {
    package_dir: PathBuf,
    manifest: Manifest,
    function: String,
    registered_name: String,
    description: String,
    parameters_schema: serde_json::Value,
    permissions: HashSet<runtime::Permission>,
    /// Shared mirror of all addon tool names for the hidden list.
    addon_tool_names: Arc<RwLock<HashSet<String>>>,
    host: u64,
}

#[async_trait::async_trait]
impl Tool for AddonTool {
    fn name(&self) -> &str {
        &self.registered_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.parameters_schema.clone()
    }

    async fn call(
        &self,
        args: &[Value],
        ctx: &mut crate::execution::context::ExecutionContext,
    ) -> DaemonResult<Value> {
        let input = serde_json::json!({"args": args_to_object(args)}).to_string();
        let call_context = Arc::new(runtime::InvocationContext {
            runtime: tokio::runtime::Handle::current(),
            permissions: self.permissions.clone(),
            addon_tool_names: self.addon_tool_names.read().clone(),
            registry: ctx.registry.clone(),
            workspace_fs: Some(Arc::new(crate::workspace::fs::WorkspaceFs::new(
                ctx.workspace_root.clone(),
            ))),
            transaction_log: ctx.transaction_log.clone(),
            llm_factory: ctx.llm_factory.clone(),
            config: ctx.config.clone(),
            audit: ctx.audit.clone(),
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .map_err(|err| DaemonError::Addon(err.to_string()))?,
        });
        let package_dir = self.package_dir.clone();
        let manifest = self.manifest.clone();
        let function = self.function.clone();
        let fallback = self.host;
        let result = tokio::task::spawn_blocking(move || {
            runtime::invoke(&package_dir, &manifest, &function, &input, call_context, fallback)
        })
        .await
        .map_err(|err| DaemonError::Addon(format!("plugin task panicked: {err}")))?;
        let output = result?;
        let parsed: serde_json::Value = serde_json::from_str(&output)
            .map_err(|err| DaemonError::Addon(format!("invalid JSON from addon: {err}")))?;
        Ok(Value::Json(parsed))
    }
}

fn args_to_object(args: &[Value]) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    for value in args {
        if let Value::Json(serde_json::Value::Object(entries)) = value {
            for (key, item) in entries {
                map.insert(key.clone(), item.clone());
            }
        } else if let Value::String(text) = value {
            // Positional string arguments become {"input": text}.
            map.insert("input".to_string(), serde_json::Value::String(text.clone()));
        }
    }
    map
}

fn pascal(input: &str) -> String {
    input
        .split(['.', '-', '_', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

fn toml_table_to_json(table: &toml::Table) -> serde_json::Value {
    // Convert through TOML text to reuse the lossless serializer.
    toml::to_string(table)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .map(|converted| toml_value_to_json(&toml::Value::Table(converted)))
        .unwrap_or(serde_json::json!({"type": "object"}))
}

fn toml_value_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::Boolean(flag) => serde_json::Value::Bool(*flag),
        toml::Value::Integer(number) => serde_json::Value::Number((*number).into()),
        toml::Value::Float(number) => serde_json::Number::from_f64(*number)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        toml::Value::String(text) => serde_json::Value::String(text.clone()),
        toml::Value::Datetime(text) => serde_json::Value::String(text.to_string()),
        toml::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(map) => {
            let mut object = serde_json::Map::new();
            for (key, item) in map {
                object.insert(key.clone(), toml_value_to_json(item));
            }
            serde_json::Value::Object(object)
        }
    }
}

fn has_zip_extension(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

/// Extracts a zip archive into `target`, rejecting path escapes (zip-slip).
fn unzip_into(archive_path: &Path, target: &Path) -> DaemonResult<()> {
    let file = std::fs::File::open(archive_path)
        .map_err(|err| DaemonError::NotFound(format!("cannot open package: {err}")))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|err| DaemonError::Addon(format!("bad zip: {err}")))?;
    std::fs::create_dir_all(target).map_err(DaemonError::Io)?;
    let canonical_target = canonicalize_or_create(target)?;

    for index in 0..archive.len() {
        let mut entry =
            archive.by_index(index).map_err(|err| DaemonError::Addon(err.to_string()))?;
        let relative = entry.name().replace('\\', "/");
        let dest = canonical_target.join(&relative);
        if !dest.starts_with(&canonical_target) {
            return Err(DaemonError::Addon(format!("unsafe zip entry: {}", entry.name())));
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&dest).map_err(DaemonError::Io)?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(DaemonError::Io)?;
        }
        let mut out = std::fs::File::create(&dest).map_err(DaemonError::Io)?;
        std::io::copy(&mut entry, &mut out).map_err(DaemonError::Io)?;
    }
    Ok(())
}

/// Copies an unpacked package directory into `target/<dirname>`.
fn copy_tree(source: &Path, dest: &Path) -> DaemonResult<()> {
    std::fs::create_dir_all(dest).map_err(DaemonError::Io)?;
    for entry in std::fs::read_dir(source).map_err(DaemonError::Io)? {
        let entry = entry.map_err(DaemonError::Io)?;
        let entry_dest = dest.join(entry.file_name());
        if entry.file_type().map_err(DaemonError::Io)?.is_dir() {
            copy_tree(&entry.path(), &entry_dest)?;
        } else {
            std::fs::copy(entry.path(), &entry_dest).map_err(DaemonError::Io)?;
        }
    }
    Ok(())
}

fn canonicalize_or_create(path: &Path) -> DaemonResult<PathBuf> {
    std::fs::create_dir_all(path).map_err(DaemonError::Io)?;
    path.canonicalize().map_err(DaemonError::Io)
}

fn list_addon_dirs(base: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    entries.filter_map(Result::ok).map(|entry| entry.path()).filter(|path| path.is_dir()).collect()
}

/// Finds the addon directory whose manifest id (or, as a fallback, directory
/// name) equals `id`. Exact matches only: prefix matching would let
/// `com.a.foo` resolve to a `com.a.foobar` installation.
fn match_addon_dir(base: &Path, id: &str) -> Option<PathBuf> {
    let mut by_name = None;
    for dir in list_addon_dirs(base) {
        if let Ok((manifest, _)) = Manifest::load(&dir) {
            if manifest.id == id {
                return Some(dir);
            }
            continue;
        }
        if by_name.is_none()
            && dir.file_name().and_then(|name| name.to_str()).is_some_and(|name| name == id)
        {
            by_name = Some(dir);
        }
    }
    by_name
}

fn read_granted(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("granted.toml"))
        .ok()
        .and_then(|text| toml::from_str::<GrantedFile>(&text).ok())
        .map(|file| file.granted)
        .unwrap_or_default()
}

/// The on-disk format of `granted.toml`.
#[derive(serde::Serialize, serde::Deserialize)]
struct GrantedFile {
    granted: Vec<String>,
}

fn toml_string(items: &[String]) -> String {
    toml::to_string(&GrantedFile {
        granted: items.to_vec(),
    })
    .unwrap_or_default()
}
