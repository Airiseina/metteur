//! Extism plugin invocation with permission-gated host functions.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use extism::{Manifest as ExtismManifest, PTR, PluginBuilder, UserData, Wasm};
use serde_json::Value;

use crate::error::{DaemonError, DaemonResult};

use super::manifest::Manifest;

/// Default plugin-call timeout when neither config nor manifest set one.
const DEFAULT_CALL_TIMEOUT_MS: u64 = 30_000;

/// Permissions an addon can hold (doc §6.4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    FsRead,
    FsWrite,
    Network,
    Llm,
    Tools,
}

impl Permission {
    /// Parses a manifest permission string.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "fs:read" => Some(Self::FsRead),
            "fs:write" => Some(Self::FsWrite),
            "network" => Some(Self::Network),
            "llm" => Some(Self::Llm),
            "tools" => Some(Self::Tools),
            _ => None,
        }
    }
}

/// Handles handed to host functions for one plugin invocation.
pub struct InvocationContext {
    /// Runtime handle captured before entering `spawn_blocking`.
    pub runtime: tokio::runtime::Handle,
    pub permissions: HashSet<Permission>,
    /// Registry names of all addon tools, hidden from `call_tool` (doc §6.4.5).
    pub addon_tool_names: HashSet<String>,
    pub registry: Arc<crate::registry::Registry>,
    pub workspace_fs: Option<Arc<crate::fs::WorkspaceFs>>,
    pub transaction_log: crate::execution::transaction::TransactionLog,
    pub llm_factory: crate::llm::LlmClientFactory,
    pub config: Option<Arc<tokio::sync::RwLock<metteur_shared::config::Config>>>,
    pub audit: Option<crate::audit::AuditWriter>,
    pub http: reqwest::blocking::Client,
}

fn denied(what: &str) -> extism::Error {
    extism::Error::msg(format!("permission denied: {what}"))
}

fn require(
    perms: &HashSet<Permission>,
    permission: Permission,
    what: &str,
) -> Result<(), extism::Error> {
    if perms.contains(&permission) {
        Ok(())
    } else {
        Err(denied(what))
    }
}

fn host_error(err: impl std::fmt::Display) -> extism::Error {
    extism::Error::msg(err.to_string())
}

/// Tools that addons may never reach through `call_tool` (doc §6.4.5).
fn is_hidden_from_addons(name: &str, addon_tools: &HashSet<String>) -> bool {
    matches!(name, "ExecuteCommand" | "SpawnSubAgent") || addon_tools.contains(name)
}

/// Shared per-invocation handle passed to every host function.
type Ctx = Arc<InvocationContext>;

fn shared(user_data: UserData<Ctx>) -> Result<Ctx, extism::Error> {
    let inner = user_data.get().map_err(host_error)?;
    inner.lock().map(|guard| guard.clone()).map_err(|_| extism::Error::msg("context poisoned"))
}

extism::host_fn!(hf_log(user_data: Ctx; level: String, message: String) -> () {
    let _ = user_data;
    match level.as_str() {
        "warn" => tracing::warn!(target: "addon", "{message}"),
        "error" => tracing::error!(target: "addon", "{message}"),
        _ => tracing::info!(target: "addon", "{message}"),
    }
    Ok(())
});

extism::host_fn!(hf_call_tool(user_data: Ctx; name: String, args_json: String) -> String {
    let ctx = shared(user_data)?;
    require(&ctx.permissions, Permission::Tools, "tools")?;
    if is_hidden_from_addons(&name, &ctx.addon_tool_names) {
        return Err(extism::Error::msg(format!("tool '{name}' is not available to addons")));
    }
    let Some(tool) = ctx.registry.tool(&name) else {
        return Err(extism::Error::msg(format!("unknown tool '{name}'")));
    };
    let args: Value = serde_json::from_str(&args_json)
        .map_err(|err| extism::Error::msg(format!("invalid tool arguments JSON: {err}")))?;
    let object = match args {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let mut exec_ctx = build_exec_context(&ctx).map_err(host_error)?;
    let values = vec![metteur_shared::Value::Json(Value::Object(object))];
    let result = ctx.runtime.block_on(tool.call(&values, &mut exec_ctx));
    let result = result.map_err(host_error)?;
    Ok(serde_json::to_string(&value_to_json(&result)).unwrap_or_else(|_| "null".to_string()))
});

extism::host_fn!(hf_fs_read(user_data: Ctx; path: String) -> Vec<u8> {
    let ctx = shared(user_data)?;
    require(&ctx.permissions, Permission::FsRead, "fs:read")?;
    let fs = ctx.workspace_fs.as_ref().ok_or_else(|| {
        extism::Error::msg("filesystem access requires an active workspace")
    })?;
    fs.read(&path).map_err(host_error)
});

extism::host_fn!(hf_fs_write(user_data: Ctx; path: String, data: Vec<u8>) -> () {
    let ctx = shared(user_data)?;
    require(&ctx.permissions, Permission::FsWrite, "fs:write")?;
    let fs = ctx.workspace_fs.as_ref().ok_or_else(|| {
        extism::Error::msg("filesystem access requires an active workspace")
    })?;
    // Capture prior content so the write stays reversible.
    let old = fs.read(&path).ok();
    fs.write(&path, &data).map_err(host_error)?;
    ctx.transaction_log.record_file_write(PathBuf::from(&path), old, data);
    Ok(())
});

extism::host_fn!(hf_http_request(
    user_data: Ctx;
    method: String,
    url: String,
    headers_json: String,
    body: Vec<u8>
) -> String {
    let ctx = shared(user_data)?;
    require(&ctx.permissions, Permission::Network, "network")?;
    let headers: Value = serde_json::from_str(&headers_json).unwrap_or(Value::Null);
    let mut request = match method.to_uppercase().as_str() {
        "POST" => ctx.http.post(&url),
        "PUT" => ctx.http.put(&url),
        "DELETE" => ctx.http.delete(&url),
        "PATCH" => ctx.http.patch(&url),
        _ => ctx.http.get(&url),
    };
    if let Value::Object(map) = headers {
        for (key, value) in map {
            if let Some(text) = value.as_str() {
                request = request.header(key, text);
            }
        }
    }
    let response = request.body(body).send().map_err(host_error)?;
    let status = response.status().as_u16();
    let body_bytes = response.bytes().map_err(host_error)?;
    Ok(serde_json::json!({
        "status": status,
        "body_base64": base64_encode(&body_bytes),
    })
    .to_string())
});

extism::host_fn!(hf_llm_complete(
    user_data: Ctx;
    provider: String,
    model: String,
    messages_json: String
) -> String {
    let ctx = shared(user_data)?;
    require(&ctx.permissions, Permission::Llm, "llm")?;
    let context = parse_messages(&messages_json)
        .map_err(|err| extism::Error::msg(format!("invalid messages JSON: {err}")))?;
    let mut exec_ctx = build_exec_context(&ctx).map_err(host_error)?;
    let opts = crate::execution::react::ReactOptions {
        provider: if provider.is_empty() { "openai-chat".to_string() } else { provider },
        model: resolve_default_model(&exec_ctx, model),
        allowed_tools: Some(HashSet::new()),
        label: "AddonLLM".to_string(),
        ..Default::default()
    };
    let outcome = ctx.runtime.block_on(crate::execution::react::run_react(
        &mut exec_ctx,
        context,
        &opts,
    ));
    match outcome {
        Ok(result) => Ok(result.text),
        Err(err) => Err(extism::Error::msg(err.to_string())),
    }
});

fn build_exec_context(
    ctx: &InvocationContext,
) -> DaemonResult<crate::execution::context::ExecutionContext> {
    let mut exec_ctx = crate::execution::context::ExecutionContext::new(
        ctx.registry.clone(),
        ctx.llm_factory.clone(),
        ctx.workspace_fs.as_ref().map(|fs| fs.root().to_path_buf()).unwrap_or_default(),
    );
    if let Some(config) = &ctx.config {
        exec_ctx.config = Some(config.clone());
    }
    if let Some(audit) = &ctx.audit {
        exec_ctx.audit = Some(audit.clone());
    }
    Ok(exec_ctx)
}

fn resolve_default_model(
    exec_ctx: &crate::execution::context::ExecutionContext,
    explicit: String,
) -> Option<String> {
    if !explicit.is_empty() {
        return Some(explicit);
    }
    let config = exec_ctx.config.as_ref()?;
    let llm = &config.blocking_read().llm;
    llm.subagent_default_model.clone().or_else(|| llm.default_model.clone())
}

fn parse_messages(
    messages_json: &str,
) -> Result<metteur_shared::llm::ContextManager, serde_json::Error> {
    use metteur_shared::llm::{ContextManager, Message, Role};
    #[derive(serde::Deserialize)]
    struct RawMessage {
        role: String,
        content: String,
    }
    let raw: Vec<RawMessage> = serde_json::from_str(messages_json)?;
    let mut manager = ContextManager::default();
    for item in raw {
        let role = match item.role.as_str() {
            "assistant" => Role::Assistant,
            "system" => Role::System,
            "tool" => Role::Tool,
            _ => Role::User,
        };
        manager.push_message(Message::text(role, item.content));
    }
    Ok(manager)
}

fn value_to_json(value: &metteur_shared::Value) -> Value {
    match value {
        metteur_shared::Value::Null => Value::Null,
        metteur_shared::Value::Bool(flag) => Value::Bool(*flag),
        metteur_shared::Value::Int(number) => Value::Number((*number).into()),
        metteur_shared::Value::Float(number) => {
            serde_json::Number::from_f64(*number).map_or(Value::Null, Value::Number)
        }
        metteur_shared::Value::String(text) => Value::String(text.clone()),
        metteur_shared::Value::List(items) => {
            Value::Array(items.iter().map(value_to_json).collect())
        }
        metteur_shared::Value::Json(json) => json.clone(),
        // Contexts cannot cross back into the plugin boundary.
        metteur_shared::Value::Context(_) => Value::Null,
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(triple >> 18) as usize & 0x3F] as char);
        out.push(TABLE[(triple >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(triple >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[triple as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

/// Invokes one exported addon function with JSON input/output.
///
/// The plugin is instantiated per call: stateless, timeout-enforced and safe
/// under concurrency (doc §6.4.4).
pub fn invoke(
    package_dir: &Path,
    manifest: &Manifest,
    function: &str,
    input_json: &str,
    call_context: Arc<InvocationContext>,
    fallback_timeout_ms: u64,
) -> DaemonResult<String> {
    let entry = package_dir.join(&manifest.addon.entry);
    let mut wasm_manifest = ExtismManifest::new(vec![Wasm::file(entry)]);
    let fallback = if fallback_timeout_ms == 0 {
        DEFAULT_CALL_TIMEOUT_MS
    } else {
        fallback_timeout_ms
    };
    wasm_manifest.timeout_ms = Some(manifest.call_timeout_ms(fallback).min(u64::from(u32::MAX)));
    let user_data = UserData::new(call_context);

    let mut plugin = PluginBuilder::new(wasm_manifest)
        .with_wasi(true)
        .with_function("log", [PTR, PTR], [], user_data.clone(), hf_log)
        .with_function("call_tool", [PTR, PTR], [PTR], user_data.clone(), hf_call_tool)
        .with_function("fs_read", [PTR], [PTR], user_data.clone(), hf_fs_read)
        .with_function("fs_write", [PTR, PTR], [], user_data.clone(), hf_fs_write)
        .with_function(
            "http_request",
            [PTR, PTR, PTR, PTR],
            [PTR],
            user_data.clone(),
            hf_http_request,
        )
        .with_function("llm_complete", [PTR, PTR, PTR], [PTR], user_data, hf_llm_complete)
        .build()
        .map_err(|err| DaemonError::Addon(format!("plugin init failed: {err}")))?;

    let output: Vec<u8> = plugin
        .call(function, input_json)
        .map_err(|err| DaemonError::Addon(format!("addon call '{function}' failed: {err}")))?;
    String::from_utf8(output)
        .map_err(|err| DaemonError::Addon(format!("addon returned invalid UTF-8: {err}")))
}
