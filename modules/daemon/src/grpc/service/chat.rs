//! Chat RPCs: streaming conversation turns, abort and session persistence.

use std::path::PathBuf;
use std::sync::Arc;

use metteur_shared::llm::{ContextManager, Message, Role};
use tonic::{Request, Response, Status};

use crate::chat::session::{
    ChatSessionRecord, delete_active, display_message_count, load_active, save_active, title_of,
};
use crate::execution::context::ExecutionContext;
use crate::execution::interrupt::{Interrupt, InterruptBus, InterruptPriority};
use crate::execution::react::{ReactEvent, run_react_streaming};
use crate::observability::audit::AuditWriter;
use crate::sandbox::approval::ApprovalBroker;

use super::super::acl::subject_from_request;
use super::super::proto::{
    AbortChatRequest, ChatEvent, ChatSessionInfo, ChatSessionList, DeleteChatSessionRequest,
    Empty, GetChatSessionRequest, GetChatSessionResponse, ListChatSessionsRequest, SendChatRequest,
};
use super::*;

impl DaemonService {
    pub(crate) async fn send_chat(
        &self,
        request: Request<SendChatRequest>,
    ) -> Result<
        Response<tokio_stream::wrappers::UnboundedReceiverStream<Result<ChatEvent, Status>>>,
        Status,
    > {
        let subject = subject_from_request(&request).unwrap_or_else(|| "local".to_string());
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let run_id = uuid::Uuid::new_v4();
        let now = chrono::Utc::now().timestamp_millis() as u64;

        // A workspace hosts either one execution or one chat at a time.
        {
            let running = self.state.running.read().await;
            if running.contains_key(&ws_key) {
                return Err(Status::failed_precondition("workspace has a running execution"));
            }
        }
        let mut chats = self.state.chats.write().await;
        if chats.contains_key(&ws_key) {
            return Err(Status::failed_precondition("workspace already has an active chat"));
        }
        let interrupt_bus = InterruptBus::new();
        let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let broker = Arc::new(ApprovalBroker::new());
        let persist = Arc::new(std::sync::atomic::AtomicBool::new(true));
        chats.insert(
            ws_key.clone(),
            ChatRun {
                interrupt_bus: Some(interrupt_bus.clone()),
                cancel_requested: cancel_flag.clone(),
                approvals: Some(broker.clone()),
                persist: persist.clone(),
            },
        );
        drop(chats);

        let ws_db = ws.db.clone();
        // Resolve the session before scheduling the task: an explicitly named
        // session must exist and belong to this workspace; an empty id reuses
        // the active session (creating one when none exists).
        let active = load_active(&ws_db).map_err(|e| Status::internal(e.to_string()))?;
        let existing = match req.session_id.as_str() {
            "" => active,
            id => match active {
                Some(record) if record.session_id.to_string() == id => Some(record),
                _ => return Err(Status::not_found(format!("chat session {id} not found"))),
            },
        };
        let session_id = existing.as_ref().map(|r| r.session_id).unwrap_or_else(uuid::Uuid::new_v4);
        let created_at = existing.as_ref().map(|r| r.created_at).unwrap_or(now);
        let old_turns = existing.as_ref().map(|r| r.turns).unwrap_or(0);
        let title = existing.as_ref().and_then(|r| r.title.clone());

        let addon_fragments = match &self.state.addon_host {
            Some(host) => host.fragments_for(ws.root()).await,
            None => Vec::new(),
        };
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<Result<ChatEvent, Status>>();
        let state = self.state.clone();
        let registry = self.state.registry.clone();
        let llm_factory = self.state.llm_factory.clone();
        let root = ws.root().to_path_buf();
        let ws_config = ws.config.clone();
        let lsp = ws.lsp_manager.clone();
        let user = subject.clone();
        let message = req.message.clone();
        let history_json = req.history_json.clone();
        let options_json = req.options_json.clone();

        tokio::spawn(async move {
            let mut ctx = ExecutionContext::new(registry, llm_factory, root.clone())
                .with_run(run_id, now)
                .with_user(user)
                .with_config(ws_config)
                .with_audit(AuditWriter::new(ws_db.clone()));
            ctx.interrupts = Some(interrupt_bus);
            ctx.cancel_requested = cancel_flag.clone();
            ctx.approvals = Some(broker);
            ctx.workspace_db = Some(ws_db.clone());
            ctx.lsp = lsp;
            if let Some(global_db) = state.global_db.clone() {
                ctx.global_db = Some(global_db);
            }
            ctx.addon_fragments = addon_fragments.clone();

            // Compose the context: persisted sessions are authoritative (they
            // retain tool results); new sessions start from history_json.
            let mut context = match existing {
                Some(record) => record.context,
                None => {
                    let mut fresh = ContextManager {
                        system_fragments: addon_fragments,
                        ..ContextManager::default()
                    };
                    history_messages(&history_json, &mut fresh.messages);
                    fresh
                }
            };
            context.push_message(Message::text(Role::User, message));

            // New sessions take their title from the first user message.
            let title = title.or_else(|| title_of(&context));

            // Announce the session id so the client can resume later turns.
            let _ = event_tx.send(Ok(ChatEvent {
                kind: "session".to_string(),
                content: String::new(),
                detail_json: serde_json::json!({
                    "session_id": session_id.to_string(),
                    "created_at": created_at,
                })
                .to_string(),
            }));

            let opts = chat_options(&options_json);
            let delta_tx = event_tx.clone();
            let delta_cancel = cancel_flag.clone();
            let mut on_delta = move |text: String| {
                let event = ChatEvent {
                    kind: "assistant_delta".to_string(),
                    content: text,
                    detail_json: String::new(),
                };
                if delta_tx.send(Ok(event)).is_err() {
                    delta_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            };
            let stream_tx = event_tx.clone();
            let mut on_event = move |ev: ReactEvent| {
                let event = match ev {
                    ReactEvent::Assistant { text } => ChatEvent {
                        kind: "assistant".to_string(),
                        content: text,
                        detail_json: String::new(),
                    },
                    ReactEvent::Tool { name, content } => ChatEvent {
                        kind: "tool".to_string(),
                        content,
                        detail_json: serde_json::json!({ "name": name }).to_string(),
                    },
                };
                if stream_tx.send(Ok(event)).is_err() {
                    cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            };

            let outcome =
                run_react_streaming(&mut ctx, context, &opts, Some(&mut on_delta), &mut on_event)
                    .await;
            let (terminal, saved) = match outcome {
                Ok(outcome) => {
                    // Text-only answers are not appended by the loop; persist
                    // them so a resumed session remembers prior turns.
                    let mut saved_context = outcome.context.clone();
                    if !outcome.text.is_empty() {
                        saved_context.push_message(Message::text(
                            Role::Assistant,
                            outcome.text.clone(),
                        ));
                    }
                    let record = ChatSessionRecord {
                        session_id,
                        created_at,
                        updated_at: now,
                        turns: old_turns + 1,
                        title,
                        context: saved_context,
                    };
                    let terminal = Ok(ChatEvent {
                        kind: "done".to_string(),
                        content: String::new(),
                        detail_json: serde_json::json!({
                            "usage": {
                                "input_tokens": outcome.usage.input_tokens,
                                "output_tokens": outcome.usage.output_tokens,
                                "total_tokens": outcome.usage.total_tokens,
                            },
                            "session_id": session_id.to_string(),
                        })
                        .to_string(),
                    });
                    (terminal, Some(record))
                }
                Err((err, partial)) => {
                    // Persist the partially mutated context so an aborted or
                    // failed turn can be resumed from where it stopped.
                    let record = ChatSessionRecord {
                        session_id,
                        created_at,
                        updated_at: now,
                        turns: old_turns,
                        title,
                        context: partial,
                    };
                    let terminal = Ok(ChatEvent {
                        kind: "error".to_string(),
                        content: err.to_string(),
                        detail_json: String::new(),
                    });
                    (terminal, Some(record))
                }
            };
            if persist.load(std::sync::atomic::Ordering::SeqCst)
                && let Some(record) = saved
                && let Err(err) = save_active(&ws_db, &record)
            {
                tracing::warn!("[chat] failed to persist session: {err}");
            }
            let _ = event_tx.send(terminal);
            state.chats.write().await.remove(&ws_key);
        });

        Ok(Response::new(tokio_stream::wrappers::UnboundedReceiverStream::new(event_rx)))
    }

    pub(crate) async fn abort_chat(
        &self,
        request: Request<AbortChatRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let chats = self.state.chats.read().await;
        let entry = chats.get(&ws_key).ok_or_else(|| Status::not_found("no active chat"))?;
        entry.cancel_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(bus) = &entry.interrupt_bus {
            bus.send(Interrupt {
                priority: InterruptPriority::Emergency,
                message: "Aborted by user.".to_string(),
            });
        }
        Ok(Response::new(Empty {}))
    }

    pub(crate) async fn list_chat_sessions(
        &self,
        request: Request<ListChatSessionsRequest>,
    ) -> Result<Response<ChatSessionList>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let sessions = match load_active(&ws.db).map_err(|e| Status::internal(e.to_string()))? {
            Some(record) => vec![ChatSessionInfo {
                session_id: record.session_id.to_string(),
                created_at: record.created_at as i64,
                updated_at: record.updated_at as i64,
                turns: record.turns,
                title: record.title.clone().unwrap_or_default(),
                message_count: display_message_count(&record) as u64,
            }],
            None => Vec::new(),
        };
        Ok(Response::new(ChatSessionList { sessions }))
    }

    pub(crate) async fn get_chat_session(
        &self,
        request: Request<GetChatSessionRequest>,
    ) -> Result<Response<GetChatSessionResponse>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let record = match load_active(&ws.db).map_err(|e| Status::internal(e.to_string()))? {
            Some(record)
                if req.session_id.is_empty() || record.session_id.to_string() == req.session_id =>
            {
                record
            }
            _ => return Err(Status::not_found("chat session not found")),
        };
        Ok(Response::new(GetChatSessionResponse {
            session_id: record.session_id.to_string(),
            created_at: record.created_at as i64,
            history_json: context_history_json(&record.context),
        }))
    }

    pub(crate) async fn delete_chat_session(
        &self,
        request: Request<DeleteChatSessionRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        // Stop a running chat and forbid its finalize from persisting, so a
        // late write cannot resurrect the cleared session.
        if let Some(chat) = self.state.chats.write().await.get_mut(&ws_key) {
            chat.persist.store(false, std::sync::atomic::Ordering::SeqCst);
            chat.cancel_requested.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(bus) = &chat.interrupt_bus {
                bus.send(Interrupt {
                    priority: InterruptPriority::Emergency,
                    message: "Aborted by user.".to_string(),
                });
            }
        }
        delete_active(&ws.db).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(Empty {}))
    }
}

/// Serializes a context's user/assistant text as `[{role, content}]`.
fn context_history_json(context: &ContextManager) -> String {
    let entries: Vec<serde_json::Value> = context
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                _ => "assistant",
            };
            serde_json::json!({ "role": role, "content": m.text_content() })
        })
        .collect();
    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ContextManager {
        let mut ctx = ContextManager::default();
        ctx.push_message(Message::text(Role::User, "hello"));
        ctx.push_message(Message::text(Role::Assistant, "reply"));
        ctx.push_message(Message {
            role: Role::Tool,
            content: vec![metteur_shared::llm::ContentBlock::Text("tool out".to_string())],
            tool_calls: Vec::new(),
            tool_call_id: Some("call_1".to_string()),
        });
        ctx
    }

    #[test]
    fn history_json_excludes_tool_and_system_messages() {
        let json = context_history_json(&context());
        let entries: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["role"], "user");
        assert_eq!(entries[1]["content"], "reply");
    }
}