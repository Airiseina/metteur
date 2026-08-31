//! ratatui TUI REPL: terminal setup and the event loop.

pub mod app;
mod ui;

use std::io::stdout;

use anyhow::Context;
use metteur_proto::proto::daemon_client::DaemonClient;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;
use tonic::transport::Channel;

use app::{AppModel, Kind};

/// Events forwarded from background tasks into the UI loop.
enum UiEvent {
    Log(Kind, String),
    Approval(String, String),
    RunStatus(String, String),
    RunDone(String),
}

/// Runs the interactive TUI until the user exits.
pub async fn run_tui(
    mut client: DaemonClient<Channel>,
    initial_workspace: Option<String>,
) -> anyhow::Result<()> {
    let mut terminal = init_terminal().context("failed to enter TUI mode")?;
    let result = event_loop(&mut terminal, &mut client, initial_workspace).await;
    restore_terminal(&mut terminal);
    result
}

fn init_terminal() -> anyhow::Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).map_err(Into::into)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) {
    let _ = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = crossterm::terminal::disable_raw_mode();
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    client: &mut DaemonClient<Channel>,
    initial_workspace: Option<String>,
) -> anyhow::Result<()> {
    let mut app = AppModel::new(5000);
    app.push_log(Kind::System, "metteur TUI ready — type 'help' for commands");
    let (tx, mut rx) = mpsc::unbounded_channel::<UiEvent>();
    let mut key_rx = spawn_key_reader();

    if let Some(path) = initial_workspace {
        open_workspace(client, &mut app, &path).await;
    }

    loop {
        // Drain everything currently queued before redrawing.
        while let Ok(event) = rx.try_recv() {
            apply_event(&mut app, event);
        }

        terminal.draw(|frame| ui::draw(frame, &app))?;

        // Handle the next key press or UI event.
        let key = tokio::select! {
            key = key_rx.recv() => key,
            Some(event) = rx.recv() => {
                apply_event(&mut app, event);
                continue;
            }
        };
        let Some(key) = key else {
            break;
        };

        if handle_key(client, &mut app, key, &tx).await? {
            break;
        }
    }
    Ok(())
}

/// Applies one UI event to the model.
fn apply_event(app: &mut AppModel, event: UiEvent) {
    match event {
        UiEvent::Log(kind, text) => app.push_log(kind, text),
        UiEvent::Approval(id, detail) => {
            app.push_log(Kind::Approval, format!("approval request {id}: {detail}"));
            app.push_approval(id, detail);
        }
        UiEvent::RunStatus(label, status) => app.set_run_status(&label, &status),
        UiEvent::RunDone(label) => app.remove_run(&label),
    }
}

/// Opens the initial workspace and reflects it in the model.
async fn open_workspace(
    client: &mut DaemonClient<Channel>,
    app: &mut AppModel,
    path: &str,
) {
    let mut state = crate::commands::SessionState {
        current_ws: None,
        auto_approve: false,
    };
    match crate::commands::dispatch(
        client,
        &mut state,
        crate::commands::Command::Open {
            path: path.to_string(),
        },
    )
    .await
    {
        Ok(crate::commands::Outcome::Printed(text)) => {
            for line in text.lines() {
                app.push_log(Kind::Info, line);
            }
        }
        Ok(_) => {}
        Err(err) => app.push_log(Kind::Error, err.to_string()),
    }
    app.current_ws = state.current_ws.clone();
    app.auto_approve = state.auto_approve;
    app.workspace = Some(path.to_string());
}

/// Returns a channel of decoded key events from a blocking reader task.
fn spawn_key_reader() -> mpsc::Receiver<crossterm::event::KeyEvent> {
    let (tx, rx) = mpsc::channel(64);
    tokio::task::spawn_blocking(move || {
        use crossterm::event::{self, Event};
        while let Ok(event) = event::read() {
            if let Event::Key(key) = event
                && tx.blocking_send(key).is_err()
            {
                break;
            }
        }
    });
    rx
}

/// Applies one key press; returns true when the session should end.
async fn handle_key(
    client: &mut DaemonClient<Channel>,
    app: &mut AppModel,
    key: crossterm::event::KeyEvent,
    tx: &mpsc::UnboundedSender<UiEvent>,
) -> anyhow::Result<bool> {
    use crossterm::event::{KeyCode, KeyModifiers};

    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        return Ok(true);
    }

    // Modal keys take precedence while an approval is on screen.
    if app.has_modal() {
        let decision = match key.code {
            KeyCode::Char('o') => Some("AllowOnce"),
            KeyCode::Char('a') => Some("AllowRun"),
            KeyCode::Char('w') => Some("AllowWorkspace"),
            KeyCode::Char('g') => Some("AllowGlobal"),
            KeyCode::Char('d') => Some("DenyOnce"),
            KeyCode::Char('D') => Some("DenyRun"),
            KeyCode::Char('x') => Some("DenyWorkspace"),
            KeyCode::Char('X') => Some("DenyGlobal"),
            KeyCode::Esc => None,
            _ => return Ok(false),
        };
        let front = app.pending.first().cloned();
        if let Some(front) = front {
            match decision {
                Some(decision) => {
                    respond(client, app, &front.request_id, decision).await;
                    app.take_front_approval();
                    app.push_log(
                        Kind::System,
                        format!("approval {} -> {}", front.request_id, decision),
                    );
                }
                None => {
                    app.take_front_approval();
                    app.push_log(Kind::System, "approval dismissed without response");
                }
            }
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Enter => {
            if let Some(command) = app.submit_input() {
                app.push_log(Kind::Info, format!("> {command}"));
                if execute_command(client, app, &command, tx).await {
                    return Ok(true);
                }
            }
        }
        KeyCode::Up => app.history_prev(),
        KeyCode::Down => app.history_next(),
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::PageUp => app.scroll_up(10),
        KeyCode::PageDown => app.scroll_down(10),
        KeyCode::Char(ch) => app.input.push(ch),
        _ => {}
    }
    Ok(false)
}

async fn respond(
    client: &mut DaemonClient<Channel>,
    app: &mut AppModel,
    request_id: &str,
    decision: &str,
) {
    let ws = app.current_ws.clone().unwrap_or_default();
    let request = metteur_proto::proto::ApprovalDecisionRequest {
        workspace_path: ws,
        request_id: request_id.to_string(),
        decision: decision.to_string(),
    };
    if let Err(err) = client.respond_approval(request).await {
        app.push_log(Kind::Error, format!("approve failed: {err}"));
    }
}

/// Dispatches one command; returns true when the session should end (`exit`).
async fn execute_command(
    client: &mut DaemonClient<Channel>,
    app: &mut AppModel,
    command: &str,
    tx: &mpsc::UnboundedSender<UiEvent>,
) -> bool {
    let cmd = match crate::commands::parse(command) {
        Ok(cmd) => cmd,
        Err(message) => {
            app.push_log(Kind::Error, message);
            return false;
        }
    };
    let mut state = crate::commands::SessionState {
        current_ws: app.current_ws.clone(),
        auto_approve: app.auto_approve,
    };
    match crate::commands::dispatch(client, &mut state, cmd).await {
        Ok(crate::commands::Outcome::Exit) => return true,
        Ok(crate::commands::Outcome::Printed(text)) => {
            for line in text.lines() {
                app.push_log(Kind::Info, line);
            }
            app.current_ws = state.current_ws.clone();
            app.auto_approve = state.auto_approve;
            app.workspace = state.current_ws.clone();
        }
        Ok(crate::commands::Outcome::Started(start)) => {
            app.set_run_status(&start.label, "running");
            spawn_stream_forwarder(start.stream, start.label.clone(), tx.clone());
        }
        Err(err) => app.push_log(Kind::Error, err.to_string()),
    }
    false
}

/// Forwards execution events into the UI until the run finishes.
fn spawn_stream_forwarder(
    mut stream: tonic::codec::Streaming<metteur_proto::proto::ExecutionEvent>,
    label: String,
    tx: mpsc::UnboundedSender<UiEvent>,
) {
    tokio::spawn(async move {
        let mut failed = false;
        while let Some(event) = stream.message().await.transpose() {
            match event {
                Ok(event) => match event.kind.as_str() {
                    "approval_request" => {
                        tx.send(UiEvent::Approval(event.message, event.detail_json)).ok();
                    }
                    _ => {
                        let text = format!("[{} {}] {}", event.node_id, event.kind, event.message);
                        tx.send(UiEvent::Log(Kind::Event, text)).ok();
                    }
                },
                Err(err) => {
                    tx.send(UiEvent::Log(Kind::Error, format!("run failed: {err}"))).ok();
                    failed = true;
                    break;
                }
            }
        }
        let status = if failed {
            "failed"
        } else {
            "finished"
        }
        .to_string();
        tx.send(UiEvent::RunStatus(label.clone(), status)).ok();
        tx.send(UiEvent::RunDone(label)).ok();
    });
}
