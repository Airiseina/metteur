//! Command parsing and gRPC dispatch for the REPL (doc §3.4 command set).
//!
//! [`parse`] is a pure function and fully unit-testable; the dispatch layer
//! only talks to the daemon client.

use metteur_proto::proto::daemon_client::DaemonClient;
use metteur_proto::proto::{
    ApprovalDecisionRequest, CancelRequest, CloseWorkspaceRequest, ContinueExecutionRequest,
    CreateSnapshotRequest, CompileDslRequest, DeleteFunctionRequest, DecompileBlueprintRequest,
    Empty, ExecuteBlueprintRequest, FunctionInfo, GetConfigRequest, GetExecutionUsageRequest,
    GetFileHistoryRequest, InstallAddonRequest, InterruptRequest, ListAddonsRequest,
    ListAuditLogRequest, ListExecutionsRequest, ListFunctionsRequest, ListSnapshotsRequest,
    LoadBlueprintRequest, LoadFunctionRequest, OpenWorkspaceRequest, PauseRequest, ResumeRequest,
    RollbackRequest, SaveBlueprintRequest, SaveFunctionRequest, SetAddonEnabledRequest,
    SetConfigRequest, UninstallAddonRequest,
};
use tonic::transport::Channel;

use crate::{bp, print};

/// Mutable REPL session state.
#[derive(Debug, Default, Clone)]
pub struct SessionState {
    /// Currently selected workspace path.
    pub current_ws: Option<String>,
    /// When true, approval requests are answered with `AllowRun` automatically.
    pub auto_approve: bool,
}

/// One parsed REPL command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    Exit,
    Status,
    Open {
        path: String,
    },
    Close {
        path: String,
    },
    Ws,
    SaveBp {
        file: String,
        id: Option<String>,
    },
    LoadBp {
        id: String,
        file: Option<String>,
    },
    Exec {
        blueprint_id: String,
    },
    Cont {
        run_id: String,
    },
    Runs,
    Cancel,
    Pause,
    Resume,
    Say {
        priority: String,
        message: String,
    },
    Approve {
        request_id: String,
        decision: String,
    },
    ApproveAuto(bool),
    Tools,
    Nodes,
    Snap {
        description: String,
        alias: Option<String>,
    },
    Snaps,
    Rollback {
        /// A snapshot id (UUID) or a snapshot alias.
        target: String,
    },
    Hist {
        path: String,
    },
    AuditWs,
    AuditGlobal,
    CfgGet {
        workspace: bool,
    },
    CfgSet {
        json: String,
        workspace: bool,
    },
    Usage {
        run_id: String,
    },
    Mcp,
    Addons,
    InstallAddon {
        path: String,
        workspace: Option<String>,
    },
    UninstallAddon {
        id: String,
        workspace: Option<String>,
    },
    SetAddonEnabled {
        id: String,
        on: bool,
        workspace: Option<String>,
    },
    FuncSave {
        name: String,
        file: String,
        workspace: bool,
    },
    FuncList {
        workspace: bool,
    },
    FuncLoad {
        name: String,
        workspace: bool,
    },
    FuncRm {
        name: String,
        workspace: bool,
    },
    BpCompile {
        file: String,
        save_to: Option<String>,
    },
    BpDecompile {
        id: String,
    },
}

/// Parses the `func` command family: save/list/load/delete functions.
fn parse_func(args: &[&str]) -> Result<Command, String> {
    let workspace = |rest: &[&str]| match rest.first() {
        None | Some(&"global") => Ok(false),
        Some(&"ws") | Some(&"workspace") => Ok(true),
        Some(other) => Err(format!("unknown scope '{other}', expected 'ws' or 'global'")),
    };
    match args {
        ["save", name, file] | ["save", name, file, "ws"] | ["save", name, file, "workspace"] => {
            Ok(Command::FuncSave {
                name: (*name).to_string(),
                file: (*file).to_string(),
                workspace: args.len() > 3,
            })
        }
        ["save", _, _, scope] => Err(format!("unknown scope '{scope}', expected 'ws' or 'global'")),
        ["list"] | ["ls"] => Ok(Command::FuncList {
            workspace: false,
        }),
        ["list", rest @ ..] | ["ls", rest @ ..] => workspace(rest).map(|w| Command::FuncList {
            workspace: w,
        }),
        ["load", name] => Ok(Command::FuncLoad {
            name: (*name).to_string(),
            workspace: false,
        }),
        ["load", name, rest @ ..] => workspace(rest).map(|w| Command::FuncLoad {
            name: (*name).to_string(),
            workspace: w,
        }),
        ["rm", name] | ["delete", name] => Ok(Command::FuncRm {
            name: (*name).to_string(),
            workspace: false,
        }),
        ["rm", name, rest @ ..] | ["delete", name, rest @ ..] => {
            workspace(rest).map(|w| Command::FuncRm {
                name: (*name).to_string(),
                workspace: w,
            })
        }
        _ => Err(
            "usage: func save <name> <file.json> [ws|global] | func list [ws|global] | \
             func load <name> [ws|global] | func rm <name> [ws|global]"
                .to_string(),
        ),
    }
}

/// Reads the workspace path for a function command, when scoped to `ws`.
fn func_ws(state: &SessionState, workspace: bool) -> anyhow::Result<String> {
    if workspace {
        Ok(require_ws(state)?)
    } else {
        Ok(String::new())
    }
}

/// Parses the `bp` command family: DSL compile/decompile.
fn parse_bp(args: &[&str]) -> Result<Command, String> {
    match args {
        ["compile", file] | ["compile", file, "save"] => Ok(Command::BpCompile {
            file: (*file).to_string(),
            save_to: None,
        }),
        ["compile", file, "save", "as", id] => Ok(Command::BpCompile {
            file: (*file).to_string(),
            save_to: Some((*id).to_string()),
        }),
        ["decompile", id] => Ok(Command::BpDecompile {
            id: (*id).to_string(),
        }),
        _ => Err("usage: bp compile <file.mbp> [save [as <id>]] | bp decompile <blueprint_id>"
            .to_string()),
    }
}

/// Result of dispatching one command.
pub enum Outcome {
    /// Text to show before the next prompt.
    Printed(String),
    /// A live execution stream started by `exec` or `cont`.
    Started(Box<StreamStart>),
    /// Leave the REPL.
    Exit,
}

/// A freshly started execution stream handed to the REPL.
pub struct StreamStart {
    /// Live server-side event stream.
    pub stream: tonic::codec::Streaming<metteur_proto::proto::ExecutionEvent>,
    /// Server-assigned run id when it could be discovered.
    pub run_id: Option<String>,
    /// Short label shown by `status` while the run is active.
    pub label: String,
}

const HELP: &str = "\
Metteur REPL commands:
  help                                  Show this help.
  exit                                  Leave the REPL.
  status                                Current workspace and active run.
  open <path> | close <path>            Open/close a workspace.
  ws                                    List open workspaces.
  save-bp <file.json> [id]              Save a blueprint JSON file.
  load-bp <id> [file.json]              Load a blueprint (print or write JSON).
  exec <blueprint_id>                   Execute a blueprint (streams events).
  cont <run_id>                         Resume a suspended run (streams events).
  runs                                  List executions of the workspace.
  cancel | pause | resume               Control the running execution.
  say <normal|urgent|emergency> <text>  Send an interrupt message.
  approve <request_id> <decision> [workspace|global]
                                        Respond to an approval request.
  approve-auto on|off                   Auto-allow approval requests during runs.
  tools | nodes                         List registered tools / node kinds.
  snap <description...> [--alias <name>]
                                        Create a workspace snapshot.
  snaps                                 List snapshots.
  rollback <snapshot_id|alias>          Restore a snapshot.
  hist <relpath>                        Show file history across snapshots.
  audit ws|global                       Show workspace or global audit log.
  cfg get [ws] | cfg set <json> [ws]    Read/update global or workspace config.
  usage <run_id>                        Token/cost usage for a run.
  mcp                                   List MCP servers.
  addons                                List installed addons.
  install <path.zip|dir> [ws|global]    Install an addon package.
  uninstall <id> [ws|global]            Remove an addon.
  addon <id> on|off                     Enable/disable an addon.
  func save <name> <file.json> [ws|global]
                                        Save a blueprint function.
  func list [ws|global]                 List registered functions.
  func load <name> [ws|global]          Print a function body as JSON.
  func rm <name> [ws|global]            Delete a function.
  bp compile <file.mbp> [save [as <id>]]
                                        Compile DSL to JSON (and save).
  bp decompile <blueprint_id>          Render a stored blueprint as DSL.
Decisions: AllowOnce|AllowRun|AllowWorkspace|AllowGlobal|DenyOnce|DenyRun|\
DenyWorkspace|DenyGlobal (shorthand: allow|deny plus a scope).";

/// Parses a command line into a [`Command`], or an error message.
pub fn parse(line: &str) -> Result<Command, String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let Some((cmd, args)) = tokens.split_first() else {
        return Err("empty command".to_string());
    };
    match cmd.to_ascii_lowercase().as_str() {
        "help" => exact(args, "help").map(|()| Command::Help),
        "exit" => exact(args, "exit").map(|()| Command::Exit),
        "status" => exact(args, "status").map(|()| Command::Status),
        "open" => one(args, "open <path>").map(|p| Command::Open {
            path: p[0].clone(),
        }),
        "close" => one(args, "close <path>").map(|p| Command::Close {
            path: p[0].clone(),
        }),
        "ws" => exact(args, "ws").map(|()| Command::Ws),
        "save-bp" => range(args, 1, 2, "save-bp <file.json> [id]").map(|a| Command::SaveBp {
            file: a[0].clone(),
            id: a.get(1).cloned(),
        }),
        "load-bp" => range(args, 1, 2, "load-bp <id> [file.json]").map(|a| Command::LoadBp {
            id: a[0].clone(),
            file: a.get(1).cloned(),
        }),
        "exec" => one(args, "exec <blueprint_id>").map(|p| Command::Exec {
            blueprint_id: p[0].clone(),
        }),
        "cont" => one(args, "cont <run_id>").map(|p| Command::Cont {
            run_id: p[0].clone(),
        }),
        "runs" => exact(args, "runs").map(|()| Command::Runs),
        "cancel" => exact(args, "cancel").map(|()| Command::Cancel),
        "pause" => exact(args, "pause").map(|()| Command::Pause),
        "resume" => exact(args, "resume").map(|()| Command::Resume),
        "say" => {
            if args.len() < 2 {
                return Err("usage: say <normal|urgent|emergency> <message...>".to_string());
            }
            Ok(Command::Say {
                priority: priority(args[0])?,
                message: args[1..].join(" "),
            })
        }
        "approve" => {
            if !(2..=3).contains(&args.len()) {
                return Err("usage: approve <request_id> <decision> [workspace|global]".to_string());
            }
            Ok(Command::Approve {
                request_id: args[0].to_string(),
                decision: decision(args[1], args.get(2).copied())?,
            })
        }
        "approve-auto" => match args {
            ["on"] | ["true"] => Ok(Command::ApproveAuto(true)),
            ["off"] | ["false"] => Ok(Command::ApproveAuto(false)),
            _ => Err("usage: approve-auto on|off".to_string()),
        },
        "tools" => exact(args, "tools").map(|()| Command::Tools),
        "nodes" => exact(args, "nodes").map(|()| Command::Nodes),
        "snap" => parse_snap(args),
        "snaps" => exact(args, "snaps").map(|()| Command::Snaps),
        "rollback" => one(args, "rollback <snapshot_id|alias>").map(|p| Command::Rollback {
            target: p[0].clone(),
        }),
        "hist" => one(args, "hist <relpath>").map(|p| Command::Hist {
            path: p[0].clone(),
        }),
        "audit" => match args {
            ["ws"] => Ok(Command::AuditWs),
            ["global"] => Ok(Command::AuditGlobal),
            _ => Err("usage: audit ws|global".to_string()),
        },
        "cfg" => parse_cfg(args),
        "usage" => one(args, "usage <run_id>").map(|p| Command::Usage {
            run_id: p[0].clone(),
        }),
        "mcp" => exact(args, "mcp").map(|()| Command::Mcp),
        "addons" => exact(args, "addons").map(|()| Command::Addons),
        "install" => {
            if args.is_empty() || args.len() > 2 {
                return Err("install <path.zip|dir> [ws|global]".to_string());
            }
            let workspace = addon_scope(args.get(1).copied())?;
            Ok(Command::InstallAddon {
                path: args[0].to_string(),
                workspace,
            })
        }
        "uninstall" => {
            if args.len() != 2 {
                return Err("uninstall <id> [ws|global]".to_string());
            }
            let workspace = addon_scope(args.get(1).copied())?;
            Ok(Command::UninstallAddon {
                id: args[0].to_string(),
                workspace,
            })
        }
        "addon" => {
            let [id, onoff] = args else {
                return Err("addon <id> on|off".to_string());
            };
            let on = match onoff.to_ascii_lowercase().as_str() {
                "on" | "enable" => true,
                "off" | "disable" => false,
                _ => return Err("addon <id> on|off".to_string()),
            };
            Ok(Command::SetAddonEnabled {
                id: (*id).to_string(),
                on,
                workspace: None,
            })
        }
        "func" => parse_func(args),
            "bp" => parse_bp(args),
            other => Err(format!("unknown command '{other}', type 'help'")),
    }
}

/// Parses `snap <description...> [--alias <name>]` (or `-a <name>`).
///
/// When only an alias is given the description falls back to the alias.
fn parse_snap(args: &[&str]) -> Result<Command, String> {
    let (description, alias) = match args.iter().position(|t| t == &"--alias" || t == &"-a") {
        Some(i) if i + 1 < args.len() => {
            let alias = args[i + 1].to_string();
            let desc = args[..i].join(" ");
            (desc, Some(alias))
        }
        Some(_) => return Err("usage: snap <description...> [--alias <name>]".to_string()),
        None => (args.join(" "), None),
    };
    if description.is_empty() && alias.is_none() {
        return Err("usage: snap <description...> [--alias <name>]".to_string());
    }
    let description = if description.is_empty() {
        alias.clone().unwrap()
    } else {
        description
    };
    Ok(Command::Snap {
        description,
        alias,
    })
}

/// Maps a scope token: `None`/`global` -> None (global), `"ws"` kept as a
/// marker resolved against the current workspace during dispatch.
fn addon_scope(token: Option<&str>) -> Result<Option<String>, String> {
    match token {
        None | Some("global") => Ok(None),
        Some(ws @ ("ws" | "workspace")) => Ok(Some((*ws).to_string())),
        Some(other) => Err(format!("unknown scope '{other}', expected 'ws' or 'global'")),
    }
}

/// Parses `cfg get [ws] | cfg set <json> [ws]`.
fn parse_cfg(args: &[&str]) -> Result<Command, String> {
    const USAGE: &str = "usage: cfg get [ws] | cfg set <json> [ws]";
    match args {
        ["get"] => Ok(Command::CfgGet {
            workspace: false,
        }),
        ["get", tok] if tok.eq_ignore_ascii_case("ws") => Ok(Command::CfgGet {
            workspace: true,
        }),
        ["set"] => Err(USAGE.to_string()),
        ["set", rest @ ..] => {
            let (tokens, workspace) = match rest.split_last() {
                Some((last, head)) if last.eq_ignore_ascii_case("ws") && !head.is_empty() => {
                    (head, true)
                }
                _ => (rest, false),
            };
            if tokens.is_empty() {
                return Err(USAGE.to_string());
            }
            let json = tokens.join(" ");
            serde_json::from_str::<serde_json::Value>(&json)
                .map_err(|e| format!("invalid config json: {e}"))?;
            Ok(Command::CfgSet {
                json,
                workspace,
            })
        }
        [] => Err(USAGE.to_string()),
        _ => Err(USAGE.to_string()),
    }
}

/// Validates an interrupt priority, returning its canonical form.
fn priority(word: &str) -> Result<String, String> {
    match word.to_ascii_lowercase().as_str() {
        "normal" => Ok("Normal".to_string()),
        "urgent" => Ok("Urgent".to_string()),
        "emergency" => Ok("Emergency".to_string()),
        _ => Err(format!("invalid priority '{word}', expected normal|urgent|emergency")),
    }
}

/// Validates an approval decision, returning the canonical proto string.
fn decision(word: &str, scope: Option<&str>) -> Result<String, String> {
    const USAGE: &str = "usage: approve <request_id> <decision> [workspace|global]";
    let normalized = word.replace(['-', '_'], "").to_ascii_lowercase();
    let full = |canonical: &str| -> Result<String, String> {
        match scope {
            Some(extra) => Err(format!("unexpected argument '{extra}'; {USAGE}")),
            None => Ok(canonical.to_string()),
        }
    };
    match normalized.as_str() {
        "allowonce" => full("AllowOnce"),
        "allowrun" => full("AllowRun"),
        "allowworkspace" => full("AllowWorkspace"),
        "allowglobal" => full("AllowGlobal"),
        "denyonce" => full("DenyOnce"),
        "denyrun" => full("DenyRun"),
        "denyworkspace" => full("DenyWorkspace"),
        "denyglobal" => full("DenyGlobal"),
        "allow" | "deny" => {
            let scope =
                scope.ok_or_else(|| format!("decision '{word}' requires a scope; {USAGE}"))?;
            let scoped = format!("{}{}", word.to_uppercase(), scope.to_uppercase());
            match scoped.as_str() {
                "ALLOWWORKSPACE" => Ok("AllowWorkspace".to_string()),
                "ALLOWGLOBAL" => Ok("AllowGlobal".to_string()),
                "DENYWORKSPACE" => Ok("DenyWorkspace".to_string()),
                "DENYGLOBAL" => Ok("DenyGlobal".to_string()),
                _ => Err(format!("invalid scope '{scope}', expected workspace|global")),
            }
        }
        _ => Err(format!("invalid decision '{word}'")),
    }
}

fn exact(args: &[&str], usage: &str) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("unexpected argument(s); usage: {usage}"))
    }
}

fn one(args: &[&str], usage: &str) -> Result<Vec<String>, String> {
    range(args, 1, 1, usage)
}

fn range(args: &[&str], min: usize, max: usize, usage: &str) -> Result<Vec<String>, String> {
    if args.len() < min || args.len() > max {
        let expected = if min == max {
            format!("{min}")
        } else {
            format!("{min}-{max}")
        };
        return Err(format!("expected {expected} argument(s); usage: {usage}"));
    }
    Ok(args.iter().map(|s| s.to_string()).collect())
}

/// Dispatches a parsed command against the daemon.
pub async fn dispatch(
    client: &mut DaemonClient<Channel>,
    state: &mut SessionState,
    cmd: Command,
) -> anyhow::Result<Outcome> {
    match cmd {
        Command::Help => Ok(Outcome::Printed(HELP.to_string())),
        Command::Exit => Ok(Outcome::Exit),
        Command::Status => Ok(Outcome::Printed(format!(
            "workspace: {}\nauto-approve: {}",
            state.current_ws.as_deref().unwrap_or("(none)"),
            if state.auto_approve {
                "on"
            } else {
                "off"
            }
        ))),
        Command::Open {
            path,
        } => Ok(Outcome::Printed(open_workspace(client, state, &path).await?)),
        Command::Close {
            path,
        } => {
            client
                .close_workspace(CloseWorkspaceRequest {
                    path: path.clone(),
                })
                .await
                .map_err(status)?;
            if state.current_ws.as_deref() == Some(path.as_str()) {
                state.current_ws = None;
            }
            Ok(Outcome::Printed(format!("closed workspace {path}")))
        }
        Command::Ws => {
            let list = client.list_workspaces(Empty {}).await.map_err(status)?.into_inner();
            Ok(Outcome::Printed(print::workspaces(&list, state.current_ws.as_deref())))
        }
        Command::SaveBp {
            file,
            id,
        } => {
            let ws = require_ws(state)?;
            let text = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", file))?;
            let mut blueprint = bp::from_json(&text)?;
            if let Some(id) = &id {
                blueprint = bp::with_id(blueprint, id)?;
            }
            client
                .save_blueprint(SaveBlueprintRequest {
                    workspace_path: ws,
                    blueprint: Some(blueprint),
                })
                .await
                .map_err(status)?;
            Ok(Outcome::Printed(format!(
                "blueprint saved: {}",
                id.unwrap_or_else(|| "(from file)".to_string())
            )))
        }
        Command::LoadBp {
            id,
            file,
        } => {
            let ws = require_ws(state)?;
            let blueprint = client
                .load_blueprint(LoadBlueprintRequest {
                    workspace_path: ws,
                    blueprint_id: id,
                })
                .await
                .map_err(status)?
                .into_inner();
            let json = bp::to_json(&blueprint)?;
            match file {
                Some(path) => {
                    std::fs::write(&path, json)
                        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path))?;
                    Ok(Outcome::Printed(format!("blueprint written to {path}")))
                }
                None => Ok(Outcome::Printed(json)),
            }
        }
        Command::Exec {
            blueprint_id,
        } => {
            let ws = require_ws(state)?;
            let known = known_runs(client, &ws).await.unwrap_or_default();
            let stream = client
                .execute_blueprint(ExecuteBlueprintRequest {
                    workspace_path: ws.clone(),
                    blueprint_id: blueprint_id.clone(),
                })
                .await
                .map_err(status)?
                .into_inner();
            let run_id = discover_run(client, &ws, known, Some(blueprint_id.as_str())).await;
            Ok(Outcome::Started(Box::new(StreamStart {
                stream,
                run_id,
                label: format!("exec {blueprint_id}"),
            })))
        }
        Command::Cont {
            run_id,
        } => {
            let ws = require_ws(state)?;
            let stream = client
                .continue_execution(ContinueExecutionRequest {
                    workspace_path: ws.clone(),
                    run_id: run_id.clone(),
                })
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Started(Box::new(StreamStart {
                stream,
                run_id: Some(run_id.clone()),
                label: format!("cont {run_id}"),
            })))
        }
        Command::Runs => {
            let ws = require_ws(state)?;
            let list = client
                .list_executions(ListExecutionsRequest {
                    workspace_path: ws,
                })
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Printed(print::executions(&list)))
        }
        Command::Cancel => {
            let ws = require_ws(state)?;
            client
                .cancel_execution(CancelRequest {
                    workspace_path: ws,
                })
                .await
                .map_err(status)?;
            Ok(Outcome::Printed("cancel requested".to_string()))
        }
        Command::Pause => {
            let ws = require_ws(state)?;
            client
                .pause_execution(PauseRequest {
                    workspace_path: ws,
                })
                .await
                .map_err(status)?;
            Ok(Outcome::Printed("pause requested".to_string()))
        }
        Command::Resume => {
            let ws = require_ws(state)?;
            client
                .resume_execution(ResumeRequest {
                    workspace_path: ws,
                })
                .await
                .map_err(status)?;
            Ok(Outcome::Printed("resume requested".to_string()))
        }
        Command::Say {
            priority,
            message,
        } => {
            let ws = require_ws(state)?;
            client
                .send_interrupt(InterruptRequest {
                    workspace_path: ws,
                    priority: priority.clone(),
                    message: message.clone(),
                })
                .await
                .map_err(status)?;
            Ok(Outcome::Printed(format!("interrupt sent ({priority}): {message}")))
        }
        Command::Approve {
            request_id,
            decision,
        } => {
            client
                .respond_approval(ApprovalDecisionRequest {
                    workspace_path: state.current_ws.clone().unwrap_or_default(),
                    request_id: request_id.clone(),
                    decision: decision.clone(),
                })
                .await
                .map_err(status)?;
            Ok(Outcome::Printed(format!("approval {request_id}: {decision}")))
        }
        Command::ApproveAuto(on) => {
            state.auto_approve = on;
            Ok(Outcome::Printed(format!(
                "auto-approve {}",
                if on {
                    "enabled"
                } else {
                    "disabled"
                }
            )))
        }
        Command::Tools => {
            let list = client.list_tools(Empty {}).await.map_err(status)?.into_inner();
            Ok(Outcome::Printed(print::tools(&list)))
        }
        Command::Nodes => {
            let list = client.list_node_kinds(Empty {}).await.map_err(status)?.into_inner();
            Ok(Outcome::Printed(print::node_kinds(&list.kinds)))
        }
        Command::Snap {
            description,
            alias,
        } => {
            let ws = require_ws(state)?;
            let info = client
                .create_snapshot(CreateSnapshotRequest {
                    workspace_path: ws,
                    description: description.clone(),
                    alias: alias.clone().unwrap_or_default(),
                })
                .await
                .map_err(status)?
                .into_inner();
            let tag = alias.as_deref().map(|a| format!(" as {a}")).unwrap_or_default();
            Ok(Outcome::Printed(format!("snapshot {} created ({description}){tag}", info.id)))
        }
        Command::Snaps => {
            let ws = require_ws(state)?;
            let list = client
                .list_snapshots(ListSnapshotsRequest {
                    workspace_path: ws,
                })
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Printed(print::snapshots(&list)))
        }
        Command::Rollback {
            target,
        } => {
            let ws = require_ws(state)?;
            // A valid UUID is treated as a snapshot id, anything else as an
            // alias so users can roll back by name.
            let is_id = target.parse::<uuid::Uuid>().is_ok();
            client
                .rollback(RollbackRequest {
                    workspace_path: ws,
                    snapshot_id: if is_id {
                        target.clone()
                    } else {
                        String::new()
                    },
                    alias: if is_id {
                        String::new()
                    } else {
                        target.clone()
                    },
                })
                .await
                .map_err(status)?;
            Ok(Outcome::Printed(format!("rolled back to {target}")))
        }
        Command::Hist {
            path,
        } => {
            let ws = require_ws(state)?;
            let history = client
                .get_file_history(GetFileHistoryRequest {
                    workspace_path: ws,
                    path: path.clone(),
                })
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Printed(print::file_history(&history, &path)))
        }
        Command::AuditWs => {
            let ws = require_ws(state)?;
            let list = client
                .list_audit_log(ListAuditLogRequest {
                    workspace_path: ws,
                })
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Printed(print::audit(&list)))
        }
        Command::AuditGlobal => {
            let list = client
                .list_audit_log(ListAuditLogRequest {
                    workspace_path: String::new(),
                })
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Printed(print::audit(&list)))
        }
        Command::CfgGet {
            workspace,
        } => {
            let cfg = client
                .get_config(GetConfigRequest {
                    workspace_path: optional_ws(state, workspace)?,
                })
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Printed(print::pretty_json(&cfg.config_json)))
        }
        Command::CfgSet {
            json,
            workspace,
        } => {
            let ws = optional_ws(state, workspace)?;
            client
                .set_config(SetConfigRequest {
                    workspace_path: ws,
                    config_json: json,
                })
                .await
                .map_err(status)?;
            Ok(Outcome::Printed("config updated".to_string()))
        }
        Command::Usage {
            run_id,
        } => {
            let ws = require_ws(state)?;
            let summary = client
                .get_execution_usage(GetExecutionUsageRequest {
                    workspace_path: ws,
                    run_id,
                })
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Printed(print::usage(&summary)))
        }
        Command::Mcp => {
            let list = client.list_mcp_servers(Empty {}).await.map_err(status)?.into_inner();
            Ok(Outcome::Printed(print::mcp_servers(&list)))
        }
        Command::Addons => {
            let list = client
                .list_addons(ListAddonsRequest::default())
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Printed(print::addons(&list)))
        }
        Command::InstallAddon {
            path,
            workspace,
        } => {
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
        Command::UninstallAddon {
            id,
            workspace,
        } => {
            let request = UninstallAddonRequest {
                id: id.clone(),
                workspace_path: resolve_scope(workspace, state)?,
            };
            client.uninstall_addon(request).await.map_err(status)?;
            Ok(Outcome::Printed(format!("uninstalled {id}")))
        }
        Command::SetAddonEnabled {
            id,
            on,
            workspace,
        } => {
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
        Command::FuncSave {
            name,
            file,
            workspace,
        } => {
            let workspace_path = func_ws(state, workspace)?;
            let text = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", file))?;
            let body = bp::from_json(&text)?;
            let info = FunctionInfo {
                id: uuid::Uuid::new_v4().to_string(),
                name: name.clone(),
                description: String::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                source: String::new(),
                updated_at: 0,
            };
            let resp = client
                .save_function(SaveFunctionRequest {
                    workspace_path,
                    info: Some(info),
                    body: Some(body),
                })
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Printed(format!(
                "function saved: {}",
                resp.info.map(|f| f.name).unwrap_or(name)
            )))
        }
        Command::FuncList {
            workspace,
        } => {
            let workspace_path = func_ws(state, workspace)?;
            let list = client
                .list_functions(ListFunctionsRequest {
                    workspace_path,
                })
                .await
                .map_err(status)?
                .into_inner();
            let mut lines = Vec::new();
            for f in list.functions {
                let inputs = f
                    .inputs
                    .iter()
                    .map(|p| format!("{}:{}", p.name, p.data_type))
                    .collect::<Vec<_>>()
                    .join(", ");
                let outputs = f
                    .outputs
                    .iter()
                    .map(|p| format!("{}:{}", p.name, p.data_type))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!(
                    "{} [{}] ({}) -> ({})\n  {}",
                    f.name, f.source, inputs, outputs, f.description
                ));
            }
            Ok(Outcome::Printed(if lines.is_empty() {
                "no functions registered".to_string()
            } else {
                lines.join("\n")
            }))
        }
        Command::FuncLoad {
            name,
            workspace,
        } => {
            let workspace_path = func_ws(state, workspace)?;
            let loaded = client
                .load_function(LoadFunctionRequest {
                    workspace_path,
                    name: name.clone(),
                })
                .await
                .map_err(status)?
                .into_inner();
            let body = loaded
                .body
                .ok_or_else(|| anyhow::anyhow!("function has no body"))?;
            Ok(Outcome::Printed(bp::to_json(&body)?))
        }
        Command::FuncRm {
            name,
            workspace,
        } => {
            let workspace_path = func_ws(state, workspace)?;
            client
                .delete_function(DeleteFunctionRequest {
                    workspace_path,
                    name: name.clone(),
                })
                .await
                .map_err(status)?;
            Ok(Outcome::Printed(format!("function deleted: {name}")))
        }
        Command::BpCompile {
            file,
            save_to,
        } => {
            let ws = require_ws(state)?;
            let source = std::fs::read_to_string(&file)
                .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", file))?;
            let blueprint = client
                .compile_dsl(CompileDslRequest {
                    source,
                })
                .await
                .map_err(status)?
                .into_inner();
            if let Some(id) = save_to {
                let blueprint = bp::with_id(blueprint, &id)?;
                client
                    .save_blueprint(SaveBlueprintRequest {
                        workspace_path: ws,
                        blueprint: Some(blueprint),
                    })
                    .await
                    .map_err(status)?;
                Ok(Outcome::Printed(format!("blueprint compiled and saved as {id}")))
            } else {
                Ok(Outcome::Printed(bp::to_json(&blueprint)?))
            }
        }
        Command::BpDecompile {
            id,
        } => {
            let ws = require_ws(state)?;
            let resp = client
                .decompile_blueprint(DecompileBlueprintRequest {
                    workspace_path: ws,
                    blueprint_id: id.clone(),
                })
                .await
                .map_err(status)?
                .into_inner();
            Ok(Outcome::Printed(resp.source))
        }
    }
}

/// Opens a workspace and records it as the current session workspace.
pub async fn open_workspace(
    client: &mut DaemonClient<Channel>,
    state: &mut SessionState,
    path: &str,
) -> anyhow::Result<String> {
    let info = client
        .open_workspace(OpenWorkspaceRequest {
            path: path.to_string(),
        })
        .await
        .map_err(status)?
        .into_inner();
    state.current_ws = Some(path.to_string());
    Ok(if info.locked {
        format!("opened workspace {} (locked)", info.path)
    } else {
        format!("opened workspace {}", info.path)
    })
}

/// Best-effort snapshot of run ids already known to the daemon.
async fn known_runs(client: &mut DaemonClient<Channel>, ws: &str) -> anyhow::Result<Vec<String>> {
    let list = client
        .list_executions(ListExecutionsRequest {
            workspace_path: ws.to_string(),
        })
        .await
        .map_err(status)?
        .into_inner();
    Ok(list.executions.into_iter().map(|e| e.run_id).collect())
}

/// Finds the newly created run for `blueprint_id`, if its checkpoint exists yet.
async fn discover_run(
    client: &mut DaemonClient<Channel>,
    ws: &str,
    known: Vec<String>,
    blueprint_id: Option<&str>,
) -> Option<String> {
    let list = client
        .list_executions(ListExecutionsRequest {
            workspace_path: ws.to_string(),
        })
        .await
        .ok()?
        .into_inner();
    list.executions
        .into_iter()
        .find(|e| !known.contains(&e.run_id) && blueprint_id.is_none_or(|b| e.blueprint_id == b))
        .map(|e| e.run_id)
}

/// Returns the current workspace path or fails with guidance.
fn require_ws(state: &SessionState) -> anyhow::Result<String> {
    state
        .current_ws
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no workspace selected; use 'open <path>'"))
}

/// Empty path for global scope, current workspace for `ws` scope.
fn optional_ws(state: &SessionState, workspace: bool) -> anyhow::Result<String> {
    if workspace {
        require_ws(state)
    } else {
        Ok(String::new())
    }
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

/// Converts a tonic status into an anyhow error.
fn status(st: tonic::Status) -> anyhow::Error {
    anyhow::anyhow!("daemon error: {st}")
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
