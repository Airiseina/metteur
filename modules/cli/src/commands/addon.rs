//! Addon install/uninstall/list/enable command handlers.

use metteur_proto::proto::daemon_client::DaemonClient;
use metteur_proto::proto::{
    InstallAddonRequest, ListAddonsRequest, SetAddonEnabledRequest, UninstallAddonRequest,
};
use tonic::transport::Channel;

use super::*;
use crate::print;

/// Handles `addons`: lists installed addons.
pub(crate) async fn handle_addons(client: &mut DaemonClient<Channel>) -> anyhow::Result<Outcome> {
    let list = client.list_addons(ListAddonsRequest::default()).await.map_err(status)?.into_inner();
    Ok(Outcome::Printed(print::addons(&list)))
}

/// Handles `install <path.zip|dir> [ws|global]`.
pub(crate) async fn handle_install_addon(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    path: String,
    workspace: Option<String>,
) -> anyhow::Result<Outcome> {
    let granted = addon_permissions_prompt(&path)?;
    let request = InstallAddonRequest {
        package_path: path,
        workspace_path: resolve_scope(workspace, state)?,
        granted_permissions: granted,
    };
    let info = client.install_addon(request).await.map_err(status)?.into_inner();
    Ok(Outcome::Printed(format!(
        "installed {} v{} ({} tool(s), {} fragment(s))",
        info.id, info.version, info.tool_count, info.fragment_count
    )))
}

/// Handles `uninstall <id> [ws|global]`.
pub(crate) async fn handle_uninstall_addon(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    id: String,
    workspace: Option<String>,
) -> anyhow::Result<Outcome> {
    let request = UninstallAddonRequest {
        id: id.clone(),
        workspace_path: resolve_scope(workspace, state)?,
    };
    client.uninstall_addon(request).await.map_err(status)?;
    Ok(Outcome::Printed(format!("uninstalled {id}")))
}

/// Handles `addon <id> on|off`.
pub(crate) async fn handle_set_addon_enabled(
    client: &mut DaemonClient<Channel>,
    state: &SessionState,
    id: String,
    on: bool,
    workspace: Option<String>,
) -> anyhow::Result<Outcome> {
    let request = SetAddonEnabledRequest {
        id: id.clone(),
        workspace_path: resolve_scope(workspace, state)?,
        enabled: on,
    };
    client.set_addon_enabled(request).await.map_err(status)?;
    Ok(Outcome::Printed(format!(
        "{id} is now {}",
        if on {
            "on"
        } else {
            "off"
        }
    )))
}

/// Resolves an addon scope token to a `workspace_path` field value.
fn resolve_scope(workspace: Option<String>, state: &SessionState) -> anyhow::Result<String> {
    match workspace.as_deref() {
        None => Ok(String::new()),
        Some("ws" | "workspace") => require_ws(state),
        Some(other) => Ok(other.to_string()),
    }
}

/// Reads the manifest of an addon package (directory or zip) as text.
fn read_addon_manifest(path: &str) -> anyhow::Result<String> {
    let package = std::path::Path::new(path);
    if package.is_dir() {
        return std::fs::read_to_string(package.join("manifest.toml"))
            .map_err(|e| anyhow::anyhow!("cannot read manifest.toml: {e}"));
    }
    let file =
        std::fs::File::open(package).map_err(|e| anyhow::anyhow!("cannot open package: {e}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| anyhow::anyhow!("not a readable zip package: {e}"))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.name().ends_with("manifest.toml") {
            use std::io::Read;
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            return Ok(text);
        }
    }
    Err(anyhow::anyhow!("package has no manifest.toml"))
}

/// Extracts the `[permissions].required` list from manifest text.
fn extract_required_permissions(text: &str) -> Vec<String> {
    let mut in_permissions = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_permissions = trimmed == "[permissions]";
            continue;
        }
        if !in_permissions {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=')
            && key.trim() == "required"
        {
            return value.split('"').skip(1).step_by(2).map(str::to_string).collect();
        }
    }
    Vec::new()
}

/// Determines the permissions to grant for an install, printing them.
fn addon_permissions_prompt(path: &str) -> anyhow::Result<Vec<String>> {
    let required = match read_addon_manifest(path) {
        Ok(text) => extract_required_permissions(&text),
        Err(err) => {
            println!("{err}; granting no permissions");
            Vec::new()
        }
    };
    if required.is_empty() {
        println!("this addon requests no permissions");
    } else {
        println!("granting permissions: {}", required.join(", "));
    }
    Ok(required)
}

#[cfg(test)]
mod addon_tests {
    use super::*;

    #[test]
    fn extracts_required_permissions_from_manifest_text() {
        let manifest = "\
id = \"com.x\"
[permissions]
required = [\"tools\", \"fs:read\"]
[addon]
entry = \"main.wasm\"
";
        assert_eq!(
            extract_required_permissions(manifest),
            vec!["tools".to_string(), "fs:read".to_string()]
        );
    }

    #[test]
    fn missing_permissions_section_yields_empty_list() {
        assert!(extract_required_permissions("id = \"com.x\"\n").is_empty());
    }
}
