//! Command parsing and gRPC dispatch for the REPL.
//!
//! [`parse`] is a pure function and fully unit-testable; the dispatch layer
//! only talks to the daemon client. Domain handlers live in the sibling
//! modules `workspace`, `blueprint`, `version`, `assets`, `config`, `audit`,
//! `approve`, `mcp` and `addon`; helpers shared by several of them
//! (`require_ws`, `status`) live here.

pub mod addon;
pub mod approve;
pub mod assets;
pub mod audit;
pub mod blueprint;
pub mod config;
pub mod mcp;
pub mod version;
pub mod workspace;

use metteur_proto::proto::daemon_client::DaemonClient;
use tonic::transport::Channel;

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
        } => workspace::handle_open(client, state, path).await,
        Command::Close {
            path,
        } => workspace::handle_close(client, state, path).await,
        Command::Ws => workspace::handle_ws(client, state).await,
        Command::SaveBp {
            file,
            id,
        } => blueprint::handle_save_bp(client, state, file, id).await,
        Command::LoadBp {
            id,
            file,
        } => blueprint::handle_load_bp(client, state, id, file).await,
        Command::Exec {
            blueprint_id,
        } => blueprint::handle_exec(client, state, blueprint_id).await,
        Command::Cont {
            run_id,
        } => blueprint::handle_cont(client, state, run_id).await,
        Command::Runs => blueprint::handle_runs(client, state).await,
        Command::Cancel => blueprint::handle_cancel(client, state).await,
        Command::Pause => blueprint::handle_pause(client, state).await,
        Command::Resume => blueprint::handle_resume(client, state).await,
        Command::Say {
            priority,
            message,
        } => blueprint::handle_say(client, state, priority, message).await,
        Command::Approve {
            request_id,
            decision,
        } => approve::handle_approve(client, state, request_id, decision).await,
        Command::ApproveAuto(on) => approve::handle_approve_auto(state, on).await,
        Command::Tools => assets::handle_tools(client).await,
        Command::Nodes => assets::handle_nodes(client).await,
        Command::Snap {
            description,
            alias,
        } => version::handle_snap(client, state, description, alias).await,
        Command::Snaps => version::handle_snaps(client, state).await,
        Command::Rollback {
            target,
        } => version::handle_rollback(client, state, target).await,
        Command::Hist {
            path,
        } => version::handle_hist(client, state, path).await,
        Command::AuditWs => audit::handle_audit_ws(client, state).await,
        Command::AuditGlobal => audit::handle_audit_global(client).await,
        Command::CfgGet {
            workspace,
        } => config::handle_cfg_get(client, state, workspace).await,
        Command::CfgSet {
            json,
            workspace,
        } => config::handle_cfg_set(client, state, json, workspace).await,
        Command::Usage {
            run_id,
        } => blueprint::handle_usage(client, state, run_id).await,
        Command::Mcp => mcp::handle_mcp(client).await,
        Command::Addons => addon::handle_addons(client).await,
        Command::InstallAddon {
            path,
            workspace,
        } => addon::handle_install_addon(client, state, path, workspace).await,
        Command::UninstallAddon {
            id,
            workspace,
        } => addon::handle_uninstall_addon(client, state, id, workspace).await,
        Command::SetAddonEnabled {
            id,
            on,
            workspace,
        } => addon::handle_set_addon_enabled(client, state, id, on, workspace).await,
        Command::FuncSave {
            name,
            file,
            workspace,
        } => assets::handle_func_save(client, state, name, file, workspace).await,
        Command::FuncList {
            workspace,
        } => assets::handle_func_list(client, state, workspace).await,
        Command::FuncLoad {
            name,
            workspace,
        } => assets::handle_func_load(client, state, name, workspace).await,
        Command::FuncRm {
            name,
            workspace,
        } => assets::handle_func_rm(client, state, name, workspace).await,
        Command::BpCompile {
            file,
            save_to,
        } => blueprint::handle_bp_compile(client, state, file, save_to).await,
        Command::BpDecompile {
            id,
        } => blueprint::handle_bp_decompile(client, state, id).await,
    }
}

/// Returns the current workspace path or fails with guidance.
pub(crate) fn require_ws(state: &SessionState) -> anyhow::Result<String> {
    state
        .current_ws
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no workspace selected; use 'open <path>'"))
}

/// Converts a tonic status into an anyhow error.
pub(crate) fn status(st: tonic::Status) -> anyhow::Error {
    anyhow::anyhow!("daemon error: {st}")
}