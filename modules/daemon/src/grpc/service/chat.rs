//! Chat RPCs: streaming conversation turns and abort.

use std::path::PathBuf;
use std::sync::Arc;

use metteur_shared::llm::{ContextManager, Message, Role};
use tonic::{Request, Response, Status};

use crate::observability::audit::AuditWriter;
use crate::execution::context::ExecutionContext;
use crate::execution::interrupt::{Interrupt, InterruptBus, InterruptPriority};
use crate::execution::react::{ReactEvent, run_react_streaming};
use crate::sandbox::approval::ApprovalBroker;

use super::super::proto::{AbortChatRequest, ChatEvent, Empty, SendChatRequest};
use super::super::acl::subject_from_request;
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
        chats.insert(
            ws_key.clone(),
            ChatRun {
                interrupt_bus: Some(interrupt_bus.clone()),
                cancel_requested: cancel_flag.clone(),
                approvals: Some(broker.clone()),
            },
        );
        drop(chats);

        let addon_fragments = match &self.state.addon_host {
            Some(host) => host.fragments_for(ws.root()).await,
            None => Vec::new(),
        };
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<Result<ChatEvent, Status>>();
        let state = self.state.clone();
        let registry = self.state.registry.clone();
        let llm_factory = self.state.llm_factory.clone();
        let root = ws.root().to_path_buf();
        let ws_db = ws.db.clone();
        let ws_config = ws.config.clone();
        let lsp = ws.lsp_manager.clone();
        let user = subject.clone();
        let message = req.message.clone();
        let history_json = req.history_json.clone();
        let options_json = req.options_json.clone();

        tokio::spawn(async move {
            let mut ctx = ExecutionContext::new(registry, llm_factory, root.clone())
                .with_run(run_id, chrono::Utc::now().timestamp_millis() as u64)
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

            // Compose the context: addon fragments, prior history, then the turn.
            let mut context = ContextManager {
                system_fragments: addon_fragments.clone(),
                ..ContextManager::default()
            };
            ctx.addon_fragments = addon_fragments;
            history_messages(&history_json, &mut context.messages);
            context.push_message(Message::text(Role::User, message));

            let opts = chat_options(&options_json);
            // Clone the sender so the closure owns its half; the original stays
            // available for the terminal `done`/`error` event.
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
                    // The reader went away; stop the loop at the next iteration.
                    cancel_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            };
            let outcome = run_react_streaming(&mut ctx, context, &opts, &mut on_event).await;
            let terminal = match outcome {
                Ok(outcome) => Ok(ChatEvent {
                    kind: "done".to_string(),
                    content: String::new(),
                    detail_json: serde_json::json!({
                        "usage": {
                            "input_tokens": outcome.usage.input_tokens,
                            "output_tokens": outcome.usage.output_tokens,
                            "total_tokens": outcome.usage.total_tokens,
                        }
                    })
                    .to_string(),
                }),
                Err(err) => Ok(ChatEvent {
                    kind: "error".to_string(),
                    content: err.to_string(),
                    detail_json: String::new(),
                }),
            };
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
}