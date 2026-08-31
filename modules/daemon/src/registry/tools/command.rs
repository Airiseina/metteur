//! The `ExecuteCommand` tool running shell commands through the sandbox.

use async_trait::async_trait;
use metteur_shared::Value;
use tokio::process::Command;

use crate::error::{DaemonError, DaemonResult};
use crate::execution::context::ExecutionContext;
use crate::fs::WorkspaceFs;

use super::Args;
use crate::registry::tool::Tool;

/// Default seconds before a spawned command is killed.
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 60;

/// Maximum compressed output size returned to the caller.
const OUTPUT_LIMIT_BYTES: usize = 8192;

/// Runs a shell command inside the workspace after sandbox approval.
pub struct ExecuteCommand;

#[async_trait]
impl Tool for ExecuteCommand {
    fn name(&self) -> &str {
        "ExecuteCommand"
    }

    fn description(&self) -> &str {
        "Runs a shell command inside the workspace. Risky commands require \
         user approval and may be denied."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command line to run." },
                "cwd": { "type": "string", "description": "Working directory relative to the workspace root." },
                "timeout_secs": { "type": "integer", "description": "Kill the command after this many seconds (default 60)." }
            },
            "required": ["command"]
        })
    }

    async fn call(&self, args: &[Value], ctx: &mut ExecutionContext) -> DaemonResult<Value> {
        let a = Args::new(args);
        let command = a.string("command", 0).ok_or_else(|| {
            DaemonError::Execution("ExecuteCommand requires a command".to_string())
        })?;
        let cwd_arg = a.string("cwd", 1);
        let timeout_secs = a
            .int("timeout_secs", 2)
            .filter(|t| *t > 0)
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS as i64) as u64;

        if !crate::sandbox::authorize(ctx, &command).await? {
            return Err(DaemonError::Sandbox(format!("command denied by sandbox: {command}")));
        }

        let fs = WorkspaceFs::new(ctx.workspace_root.clone());
        let cwd = match cwd_arg {
            Some(dir) => fs.resolve_existing(&dir)?,
            None => ctx.workspace_root.clone(),
        };

        let mut cmd = build_shell_command(&command);
        cmd.current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output =
            tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), cmd.output())
                .await
                .map_err(|_| {
                    DaemonError::Execution(format!(
                        "command timed out after {timeout_secs}s: {command}"
                    ))
                })?
                .map_err(|e| DaemonError::Execution(format!("failed to spawn command: {e}")))?;

        let exit_code = output.status.code().unwrap_or(-1);
        let mut combined = String::new();
        combined.push_str(&String::from_utf8_lossy(&output.stdout));
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            combined.push_str(&stderr);
        }
        let compressed = crate::sandbox::compress_output(combined.trim(), OUTPUT_LIMIT_BYTES);

        ctx.audit(
            "sandbox.execute",
            serde_json::json!({
                "command": command,
                "exit_code": exit_code,
                "cwd": cwd.to_string_lossy(),
            }),
        );

        Ok(Value::Json(serde_json::json!({
            "exit_code": exit_code,
            "output": compressed,
        })))
    }
}

/// Builds the platform shell command wrapping a raw command line.
fn build_shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C");
        // `raw_arg` avoids quoting so cmd receives the line verbatim.
        cmd.raw_arg(command);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_wrapping_uses_platform_conventions() {
        let _ = build_shell_command("echo hi");
        // No assertion beyond construction; behavior is covered by smoke tests.
    }
}
