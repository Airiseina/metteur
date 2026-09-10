//! Blueprint RPCs: save/load/execute with control, the function library and
//! the DSL compile/decompile endpoints.

use std::path::PathBuf;
use std::sync::Arc;

use metteur_shared::model::function::{FnPin, FunctionEntry, FunctionSource};
use tonic::{Request, Response, Status};

use crate::error::DaemonError;
use crate::execution::DbCheckpointSink;
use crate::execution::interrupt::{Interrupt, InterruptBus, InterruptPriority};
use crate::observability::audit::AuditWriter;

use super::super::acl::subject_from_request;
use super::super::proto::{
    self, Blueprint, CancelRequest, CompileDslRequest, DecompileBlueprintRequest,
    DecompileDslResponse, DeleteFunctionRequest, Empty, ExecuteBlueprintRequest, ExecutionEvent,
    FnPin as ProtoFnPin, FunctionInfo as ProtoFunctionInfo, FunctionList, InterruptRequest,
    ListFunctionsRequest, LoadBlueprintRequest, LoadFunctionRequest, LoadFunctionResponse,
    PauseRequest, ResumeRequest, SaveBlueprintRequest, SaveFunctionRequest, SaveFunctionResponse,
};
use super::*;

impl DaemonService {
    pub(crate) async fn save_blueprint(
        &self,
        request: Request<SaveBlueprintRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let proto_blueprint =
            req.blueprint.ok_or_else(|| Status::invalid_argument("blueprint is required"))?;
        let blueprint = proto_to_blueprint(&proto_blueprint).map_err(to_status)?;
        let data = serde_json::to_vec(&blueprint).map_err(|e| Status::internal(e.to_string()))?;
        ws.db
            .put(crate::storage::persistence::cf::BLUEPRINTS, blueprint.id.as_bytes(), &data)
            .map_err(to_status)?;
        Ok(Response::new(Empty {}))
    }

    pub(crate) async fn load_blueprint(
        &self,
        request: Request<LoadBlueprintRequest>,
    ) -> Result<Response<Blueprint>, Status> {
        let req = request.into_inner();
        let ws = self
            .state
            .workspaces
            .get(&PathBuf::from(&req.workspace_path))
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let id = uuid::Uuid::parse_str(&req.blueprint_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let data = ws
            .db
            .get(crate::storage::persistence::cf::BLUEPRINTS, id.as_bytes())
            .map_err(to_status)?
            .ok_or_else(|| Status::not_found("blueprint not found"))?;
        let blueprint: metteur_shared::Blueprint =
            serde_json::from_slice(&data).map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(blueprint_to_proto(&blueprint)))
    }

    pub(crate) async fn execute_blueprint(
        &self,
        request: Request<ExecuteBlueprintRequest>,
    ) -> Result<
        Response<tokio_stream::wrappers::ReceiverStream<Result<ExecutionEvent, Status>>>,
        Status,
    > {
        let subject = subject_from_request(&request).unwrap_or_else(|| "local".to_string());
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let id = uuid::Uuid::parse_str(&req.blueprint_id)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let data = ws
            .db
            .get(crate::storage::persistence::cf::BLUEPRINTS, id.as_bytes())
            .map_err(to_status)?
            .ok_or_else(|| Status::not_found("blueprint not found"))?;
        let blueprint: metteur_shared::Blueprint =
            serde_json::from_slice(&data).map_err(|e| Status::internal(e.to_string()))?;
        let run_id = uuid::Uuid::new_v4();
        let ws_key = ws.root().to_path_buf();

        let interrupt_bus = InterruptBus::new();
        let pause_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let addon_fragments = match &self.state.addon_host {
            Some(host) => host.fragments_for(ws.root()).await,
            None => Vec::new(),
        };

        let stream = spawn_execution(
            &self.state,
            ws_key,
            ws.db.clone(),
            ws.config.clone(),
            ws.root().to_path_buf(),
            self.state.registry.clone(),
            self.state.llm_factory.clone(),
            AuditWriter::new(ws.db.clone()),
            subject,
            Arc::new(DbCheckpointSink::new(ws.db.clone(), run_id)),
            blueprint,
            None,
            interrupt_bus,
            pause_flag,
            cancel_flag,
            ws.lsp_manager.clone(),
            addon_fragments,
            Some(ws.version_manager.clone()),
        )
        .await?;

        Ok(Response::new(stream))
    }

    pub(crate) async fn cancel_execution(
        &self,
        request: Request<CancelRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let running = self.state.running.read().await;
        let entry =
            running.get(&ws_key).ok_or_else(|| Status::not_found("no running execution"))?;
        entry.cancel_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(broker) = &entry.approvals {
            broker.deny_all();
        }
        Ok(Response::new(Empty {}))
    }

    pub(crate) async fn pause_execution(
        &self,
        request: Request<PauseRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let running = self.state.running.read().await;
        let entry =
            running.get(&ws_key).ok_or_else(|| Status::not_found("no running execution"))?;
        entry.pause_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(Response::new(Empty {}))
    }

    pub(crate) async fn resume_execution(
        &self,
        request: Request<ResumeRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let running = self.state.running.read().await;
        let entry =
            running.get(&ws_key).ok_or_else(|| Status::not_found("no running execution"))?;
        entry.pause_requested.store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(Response::new(Empty {}))
    }

    pub(crate) async fn send_interrupt(
        &self,
        request: Request<InterruptRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        let ws_path = PathBuf::from(&req.workspace_path);
        let ws = self
            .state
            .workspaces
            .get(&ws_path)
            .await
            .ok_or_else(|| Status::not_found("workspace not open"))?;
        let ws_key = ws.root().to_path_buf();
        let priority = match req.priority.as_str() {
            "Urgent" => InterruptPriority::Urgent,
            "Emergency" => InterruptPriority::Emergency,
            _ => InterruptPriority::Normal,
        };
        let running = self.state.running.read().await;
        let entry =
            running.get(&ws_key).ok_or_else(|| Status::not_found("no running execution"))?;
        if let Some(bus) = &entry.interrupt_bus {
            bus.send(Interrupt {
                priority,
                message: req.message,
            });
        }
        Ok(Response::new(Empty {}))
    }

    pub(crate) async fn save_function(
        &self,
        request: Request<SaveFunctionRequest>,
    ) -> Result<Response<SaveFunctionResponse>, Status> {
        let req = request.into_inner();
        let info = req.info.ok_or_else(|| Status::invalid_argument("info is required"))?;
        let body_proto = req.body.ok_or_else(|| Status::invalid_argument("body is required"))?;
        let body = proto_to_blueprint(&body_proto).map_err(to_status)?;
        let mut entry = proto_to_function(&info, body).map_err(to_status)?;
        crate::registry::library::validate(&entry).map_err(Status::invalid_argument)?;
        entry.signature =
            FunctionEntry::derive_signature(&entry.body).map_err(Status::invalid_argument)?;

        if req.workspace_path.is_empty() {
            let db = self
                .state
                .global_db
                .clone()
                .ok_or_else(|| Status::unavailable("global database is not enabled"))?;
            entry.source = FunctionSource::Global;
            crate::registry::library::save(&db, &entry).map_err(to_status)?;
            self.state.registry.register_function(entry.clone());
        } else {
            let ws = self
                .state
                .workspaces
                .get(&PathBuf::from(&req.workspace_path))
                .await
                .ok_or_else(|| Status::not_found("workspace not open"))?;
            entry.source = FunctionSource::Workspace;
            crate::registry::library::save(&ws.db, &entry).map_err(to_status)?;
            self.state.registry.register_function(entry.clone());
        }
        Ok(Response::new(SaveFunctionResponse {
            info: Some(function_to_proto(&entry)),
        }))
    }

    pub(crate) async fn list_functions(
        &self,
        request: Request<ListFunctionsRequest>,
    ) -> Result<Response<FunctionList>, Status> {
        let req = request.into_inner();
        let functions = self.state.registry.functions();
        let filtered: Vec<ProtoFunctionInfo> = functions
            .into_iter()
            .filter(|f| {
                if req.workspace_path.is_empty() {
                    f.source != FunctionSource::Workspace
                } else {
                    f.source == FunctionSource::Workspace || f.source == FunctionSource::Builtin
                }
            })
            .map(|f| function_to_proto(&f))
            .collect();
        Ok(Response::new(FunctionList {
            functions: filtered,
        }))
    }

    pub(crate) async fn load_function(
        &self,
        request: Request<LoadFunctionRequest>,
    ) -> Result<Response<LoadFunctionResponse>, Status> {
        let req = request.into_inner();
        let entry = if req.workspace_path.is_empty() {
            self.state
                .registry
                .function(&req.name)
                .filter(|f| f.source != FunctionSource::Workspace)
        } else {
            self.state.registry.function(&req.name)
        }
        .ok_or_else(|| Status::not_found(format!("function '{}' not found", req.name)))?;
        Ok(Response::new(LoadFunctionResponse {
            info: Some(function_to_proto(&entry)),
            body: Some(blueprint_to_proto(&entry.body)),
        }))
    }

    pub(crate) async fn delete_function(
        &self,
        request: Request<DeleteFunctionRequest>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        if req.workspace_path.is_empty() {
            let db = self
                .state
                .global_db
                .clone()
                .ok_or_else(|| Status::unavailable("global database is not enabled"))?;
            crate::registry::library::delete(&db, &req.name).map_err(to_status)?;
            if let Some(entry) = self.state.registry.function(&req.name)
                && entry.source == FunctionSource::Global
            {
                self.state.registry.unregister_function(&req.name);
            }
        } else {
            let ws = self
                .state
                .workspaces
                .get(&PathBuf::from(&req.workspace_path))
                .await
                .ok_or_else(|| Status::not_found("workspace not open"))?;
            crate::registry::library::delete(&ws.db, &req.name).map_err(to_status)?;
            if let Some(entry) = self.state.registry.function(&req.name)
                && entry.source == FunctionSource::Workspace
            {
                self.state.registry.unregister_function(&req.name);
            }
        }
        Ok(Response::new(Empty {}))
    }

    pub(crate) async fn compile_dsl(
        &self,
        request: Request<CompileDslRequest>,
    ) -> Result<Response<Blueprint>, Status> {
        let source = request.into_inner().source;
        let blueprint = metteur_shared::dsl::compile(&source)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        Ok(Response::new(blueprint_to_proto(&blueprint)))
    }

    pub(crate) async fn decompile_blueprint(
        &self,
        request: Request<DecompileBlueprintRequest>,
    ) -> Result<Response<DecompileDslResponse>, Status> {
        let req = request.into_inner();
        // Prefer the in-flight blueprint so the live canvas is exported without
        // depending on the archive (which may be stale or from an older format).
        let blueprint = if let Some(proto_blueprint) = req.blueprint {
            proto_to_blueprint(&proto_blueprint).map_err(to_status)?
        } else {
            let ws = self
                .state
                .workspaces
                .get(&PathBuf::from(&req.workspace_path))
                .await
                .ok_or_else(|| Status::not_found("workspace not open"))?;
            let id = uuid::Uuid::parse_str(&req.blueprint_id)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
            let data = ws
                .db
                .get(crate::storage::persistence::cf::BLUEPRINTS, id.as_bytes())
                .map_err(to_status)?
                .ok_or_else(|| Status::not_found("blueprint not found"))?;
            serde_json::from_slice(&data).map_err(|e| Status::internal(e.to_string()))?
        };
        Ok(Response::new(DecompileDslResponse {
            source: metteur_shared::dsl::decompile(&blueprint),
        }))
    }
}

/// Converts a proto blueprint into the shared model.
fn proto_to_blueprint(proto: &Blueprint) -> Result<metteur_shared::Blueprint, DaemonError> {
    let id =
        uuid::Uuid::parse_str(&proto.id).map_err(|e| DaemonError::Serialization(e.to_string()))?;
    let nodes = proto.nodes.iter().map(proto_to_node).collect::<Result<Vec<_>, _>>()?;
    let entry_node_id = uuid::Uuid::parse_str(&proto.entry_node_id)
        .map_err(|e| DaemonError::Serialization(e.to_string()))?;
    // `decompile` indexes nodes by id unconditionally; reject dangling
    // references up front instead of panicking downstream.
    if !nodes.iter().any(|n| n.id == entry_node_id) {
        return Err(DaemonError::Serialization(
            "entry node is not present in the blueprint".to_string(),
        ));
    }
    let edges = proto.edges.iter().map(proto_to_edge).collect::<Result<Vec<_>, _>>()?;
    for edge in &edges {
        if !nodes.iter().any(|n| n.id == edge.source_node)
            || !nodes.iter().any(|n| n.id == edge.target_node)
        {
            return Err(DaemonError::Serialization(
                "edge references a node missing from the blueprint".to_string(),
            ));
        }
    }
    Ok(metteur_shared::Blueprint {
        id,
        name: proto.name.clone(),
        nodes,
        edges,
        entry_node_id,
    })
}

/// Converts a proto node into the shared model.
fn proto_to_node(proto: &proto::Node) -> Result<metteur_shared::Node, DaemonError> {
    let node_type = match proto.node_type.as_str() {
        "Event" => metteur_shared::NodeType::Event,
        "Function" => metteur_shared::NodeType::Function,
        "Pure" => metteur_shared::NodeType::Pure,
        "Control" => metteur_shared::NodeType::Control,
        _ => metteur_shared::NodeType::Function,
    };
    let pins = proto.pins.iter().map(proto_to_pin).collect::<Result<Vec<_>, _>>()?;
    Ok(metteur_shared::Node {
        id: uuid::Uuid::parse_str(&proto.id)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        node_type,
        kind: proto.kind.clone(),
        position: (proto.pos_x, proto.pos_y),
        pins,
        data: serde_json::from_str(&proto.data_json).unwrap_or(serde_json::Value::Null),
    })
}

/// Converts a proto pin into the shared model.
fn proto_to_pin(proto: &proto::Pin) -> Result<metteur_shared::Pin, DaemonError> {
    let default = if proto.default_json.is_empty() {
        None
    } else {
        serde_json::from_str::<serde_json::Value>(&proto.default_json).ok()
    };
    Ok(metteur_shared::Pin {
        id: uuid::Uuid::parse_str(&proto.id)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        key: if proto.key.is_empty() {
            None
        } else {
            Some(proto.key.clone())
        },
        name: proto.name.clone(),
        pin_type: match proto.pin_type.as_str() {
            "ExecInput" => metteur_shared::PinType::ExecInput,
            "ExecOutput" => metteur_shared::PinType::ExecOutput,
            "DataInput" => metteur_shared::PinType::DataInput,
            _ => metteur_shared::PinType::DataOutput,
        },
        data_type: proto.data_type.parse().unwrap_or(metteur_shared::DataType::Void),
        default,
        optional: proto.optional,
        choices: proto.choices.clone(),
        description: if proto.description.is_empty() {
            None
        } else {
            Some(proto.description.clone())
        },
    })
}

/// Converts a proto edge into the shared model.
fn proto_to_edge(proto: &proto::Edge) -> Result<metteur_shared::Edge, DaemonError> {
    Ok(metteur_shared::Edge {
        id: uuid::Uuid::parse_str(&proto.id)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        source_node: uuid::Uuid::parse_str(&proto.source_node)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        source_pin: uuid::Uuid::parse_str(&proto.source_pin)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        target_node: uuid::Uuid::parse_str(&proto.target_node)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        target_pin: uuid::Uuid::parse_str(&proto.target_pin)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
    })
}

/// Converts a shared blueprint into the proto model.
fn blueprint_to_proto(blueprint: &metteur_shared::Blueprint) -> Blueprint {
    Blueprint {
        id: blueprint.id.to_string(),
        name: blueprint.name.clone(),
        nodes: blueprint
            .nodes
            .iter()
            .map(|n| proto::Node {
                id: n.id.to_string(),
                node_type: match n.node_type {
                    metteur_shared::NodeType::Event => "Event".to_string(),
                    metteur_shared::NodeType::Function => "Function".to_string(),
                    metteur_shared::NodeType::Pure => "Pure".to_string(),
                    metteur_shared::NodeType::Control => "Control".to_string(),
                },
                kind: n.kind.clone(),
                pos_x: n.position.0,
                pos_y: n.position.1,
                pins: n
                    .pins
                    .iter()
                    .map(|p| proto::Pin {
                        id: p.id.to_string(),
                        name: p.name.clone(),
                        pin_type: match p.pin_type {
                            metteur_shared::PinType::ExecInput => "ExecInput".to_string(),
                            metteur_shared::PinType::ExecOutput => "ExecOutput".to_string(),
                            metteur_shared::PinType::DataInput => "DataInput".to_string(),
                            metteur_shared::PinType::DataOutput => "DataOutput".to_string(),
                        },
                        data_type: p.data_type.to_string(),
                        default_json: p
                            .default
                            .as_ref()
                            .map(serde_json::Value::to_string)
                            .unwrap_or_default(),
                        optional: p.optional,
                        choices: p.choices.clone(),
                        description: p.description.clone().unwrap_or_default(),
                        key: p.key.clone().unwrap_or_default(),
                    })
                    .collect(),
                data_json: serde_json::to_string(&n.data).unwrap_or_else(|_| "null".to_string()),
            })
            .collect(),
        edges: blueprint
            .edges
            .iter()
            .map(|e| proto::Edge {
                id: e.id.to_string(),
                source_node: e.source_node.to_string(),
                source_pin: e.source_pin.to_string(),
                target_node: e.target_node.to_string(),
                target_pin: e.target_pin.to_string(),
            })
            .collect(),
        entry_node_id: blueprint.entry_node_id.to_string(),
    }
}

/// Converts a shared function entry into the proto model.
fn function_to_proto(entry: &FunctionEntry) -> ProtoFunctionInfo {
    let source = match entry.source {
        FunctionSource::Builtin => "builtin",
        FunctionSource::Global => "global",
        FunctionSource::Workspace => "workspace",
    };
    ProtoFunctionInfo {
        id: entry.id.to_string(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        inputs: entry.signature.inputs.iter().map(fn_pin_to_proto).collect(),
        outputs: entry.signature.outputs.iter().map(fn_pin_to_proto).collect(),
        source: source.to_string(),
        updated_at: 0,
    }
}

/// Converts a proto signature pin into the shared model.
fn proto_to_fn_pin(pin: &ProtoFnPin) -> Result<FnPin, DaemonError> {
    let default = if pin.default_json.is_empty() {
        None
    } else {
        serde_json::from_str::<serde_json::Value>(&pin.default_json).ok()
    };
    Ok(FnPin {
        name: pin.name.clone(),
        data_type: pin.data_type.parse().unwrap_or(metteur_shared::DataType::Void),
        description: if pin.description.is_empty() {
            None
        } else {
            Some(pin.description.clone())
        },
        default,
        optional: pin.optional,
    })
}

/// Converts a shared signature pin into the proto model.
fn fn_pin_to_proto(pin: &FnPin) -> ProtoFnPin {
    ProtoFnPin {
        name: pin.name.clone(),
        data_type: pin.data_type.to_string(),
        description: pin.description.clone().unwrap_or_default(),
        default_json: pin.default.as_ref().map(serde_json::Value::to_string).unwrap_or_default(),
        optional: pin.optional,
    }
}

/// Converts a proto function request into a shared entry.
fn proto_to_function(
    info: &ProtoFunctionInfo,
    body: metteur_shared::Blueprint,
) -> Result<FunctionEntry, DaemonError> {
    let inputs = info.inputs.iter().map(proto_to_fn_pin).collect::<Result<Vec<_>, _>>()?;
    let outputs = info.outputs.iter().map(proto_to_fn_pin).collect::<Result<Vec<_>, _>>()?;
    Ok(FunctionEntry {
        id: uuid::Uuid::parse_str(&info.id)
            .map_err(|e| DaemonError::Serialization(e.to_string()))?,
        name: info.name.clone(),
        description: info.description.clone(),
        signature: metteur_shared::model::function::FunctionSignature {
            inputs,
            outputs,
        },
        body,
        source: FunctionSource::Workspace,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use metteur_shared::PinType;

    fn node(id: &str) -> proto::Node {
        proto::Node {
            id: id.into(),
            node_type: "Pure".into(),
            kind: "Add".into(),
            pos_x: 0.0,
            pos_y: 0.0,
            pins: vec![],
            data_json: "{}".into(),
        }
    }

    fn bp(nodes: Vec<proto::Node>, edges: Vec<proto::Edge>) -> proto::Blueprint {
        let entry = nodes.first().map(|n| n.id.clone()).unwrap_or_default();
        proto::Blueprint {
            id: uuid::Uuid::new_v4().to_string(),
            name: "t".into(),
            entry_node_id: entry,
            nodes,
            edges,
        }
    }

    fn edge(src: &proto::Node, dst: &proto::Node) -> proto::Edge {
        proto::Edge {
            id: uuid::Uuid::new_v4().to_string(),
            source_node: src.id.clone(),
            source_pin: src.pins.first().map(|p| p.id.clone()).unwrap_or_default(),
            target_node: dst.id.clone(),
            target_pin: dst.pins.first().map(|p| p.id.clone()).unwrap_or_default(),
        }
    }

    #[test]
    fn parses_well_formed_blueprint() {
        let n = node(&uuid::Uuid::new_v4().to_string());
        let parsed = proto_to_blueprint(&bp(vec![n], vec![])).unwrap();
        assert_eq!(parsed.nodes.len(), 1);
    }

    #[test]
    fn rejects_missing_entry_node() {
        let n = node(&uuid::Uuid::new_v4().to_string());
        let mut blueprint = bp(vec![n], vec![]);
        blueprint.entry_node_id = uuid::Uuid::new_v4().to_string();
        assert!(proto_to_blueprint(&blueprint).is_err());
    }

    #[test]
    fn rejects_dangling_edge() {
        let n = node(&uuid::Uuid::new_v4().to_string());
        let ghost = node(&uuid::Uuid::new_v4().to_string());
        let mut edges = vec![edge(&n, &ghost)];
        // The ghost node supplies a real pin id so the error is the node ref.
        edges[0].source_pin = uuid::Uuid::new_v4().to_string();
        edges[0].target_pin = uuid::Uuid::new_v4().to_string();
        assert!(proto_to_blueprint(&bp(vec![n], edges)).is_err());
    }

    #[test]
    fn requires_uuid_ids() {
        let n = node("n-add-0");
        assert!(proto_to_blueprint(&bp(vec![n], vec![])).is_err());
    }

    #[test]
    fn rejects_empty_entry_id() {
        let n = node(&uuid::Uuid::new_v4().to_string());
        let mut blueprint = bp(vec![n], vec![]);
        blueprint.entry_node_id = String::new();
        assert!(proto_to_blueprint(&blueprint).is_err());
    }

    #[test]
    fn pin_carries_semantic_fields() {
        let mut n = node(&uuid::Uuid::new_v4().to_string());
        n.pins = vec![proto::Pin {
            id: uuid::Uuid::new_v4().to_string(),
            name: "x".into(),
            pin_type: "DataInput".into(),
            data_type: "int".into(),
            default_json: String::new(),
            optional: false,
            choices: vec![],
            description: String::new(),
            key: "x".into(),
        }];
        let parsed = proto_to_blueprint(&bp(vec![n], vec![])).unwrap();
        assert_eq!(parsed.nodes[0].pins.len(), 1);
        assert!(matches!(parsed.nodes[0].pins[0].pin_type, PinType::DataInput));
        assert_eq!(parsed.nodes[0].pins[0].name, "x");
    }
}
