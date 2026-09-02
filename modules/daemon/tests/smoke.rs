//! End-to-end smoke tests for the daemon gRPC service.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use metteur_daemon::grpc::acl::AclLayer;
use metteur_daemon::grpc::proto::daemon_client::DaemonClient;
use metteur_daemon::grpc::proto::{
    self, AbortChatRequest, CancelRequest, CloseWorkspaceRequest, CompileDslRequest,
    ContinueExecutionRequest, CreateDirRequest, CreateSnapshotRequest, DeleteFunctionRequest,
    ExecuteBlueprintRequest, FnPin, FunctionInfo, GetConfigRequest, GetExecutionUsageRequest,
    GetFileHistoryRequest, ListAuditLogRequest, ListExecutionsRequest, ListFilesRequest,
    ListFunctionsRequest, ListSnapshotsRequest, LoadFunctionRequest, OpenWorkspaceRequest,
    ReadFileRequest, RemoveFileRequest, RenameFileRequest, RollbackRequest,
    SaveBlueprintRequest, SaveFunctionRequest, SendChatRequest, SetConfigRequest, StatFileRequest,
    WriteFileRequest,
};
use metteur_daemon::grpc::{AppState, DaemonService};
use metteur_daemon::registry::Registry;
use metteur_daemon::workspace::WorkspaceManager;
use metteur_shared::config::AclRule;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity, Server};

/// A client certificate identity for mTLS tests.
#[derive(Clone)]
struct ClientIdentity {
    cert_pem: String,
    key_pem: String,
}

/// Test certificate authority and identities.
struct Pki {
    server_cert: String,
    server_key: String,
    ca_pem: String,
    client1: ClientIdentity,
    client2: ClientIdentity,
}

/// Generates a CA, a server certificate and two client identities using rcgen.
fn generate_pki() -> Pki {
    generate_pki_with_ca("metteur test ca")
}

/// Like [`generate_pki`] but names the CA certificate `ca_cn`, so the subject
/// (and the clients' issuer DN) differs between PKIs.
fn generate_pki_with_ca(ca_cn: &str) -> Pki {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};

    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.distinguished_name.push(DnType::CommonName, ca_cn.to_string());
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);

    let sign = |cn: &str, san: bool| -> (String, String) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.distinguished_name.push(DnType::CommonName, cn.to_string());
        if san {
            params.subject_alt_names.push(rcgen::SanType::IpAddress("127.0.0.1".parse().unwrap()));
        }
        let cert = params.signed_by(&key, &issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    };

    let (server_cert, server_key) = sign("localhost", true);
    let (c1, k1) = sign("client1", false);
    let (c2, k2) = sign("client2", false);
    let ca_pem = ca_cert.pem();

    Pki {
        server_cert,
        server_key,
        ca_pem,
        client1: ClientIdentity {
            cert_pem: c1,
            key_pem: k1,
        },
        client2: ClientIdentity {
            cert_pem: c2,
            key_pem: k2,
        },
    }
}

/// Starts the daemon server on an ephemeral plaintext port.
async fn start_server(config: metteur_shared::config::Config) -> (DaemonClient<Channel>, PathBuf) {
    start_server_inner(config, None, None).await.unwrap()
}

/// Starts the daemon server with mutual TLS, connecting as `identity`.
async fn start_mtls_server(
    config: metteur_shared::config::Config,
    pki: &Pki,
    identity: &ClientIdentity,
) -> Result<(DaemonClient<Channel>, PathBuf), tonic::transport::Error> {
    start_server_inner(config, Some(pki), Some(identity)).await
}

/// Starts the daemon server and returns a connected client plus the workspace.
///
/// The server is served in-process via `serve_with_incoming`. When `pki` is
/// provided, mutual TLS is enabled and the returned client presents the given
/// identity (when given); otherwise the client uses plaintext.
async fn start_server_inner(
    config: metteur_shared::config::Config,
    pki: Option<&Pki>,
    identity: Option<&ClientIdentity>,
) -> Result<(DaemonClient<Channel>, PathBuf), tonic::transport::Error> {
    let workspace = std::env::temp_dir().join(format!("metteur-smoke-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();

    let registry = Arc::new(Registry::with_builtins());
    let data_dir = workspace.join(".metteur-data");
    let addon_host = metteur_daemon::addon::AddonHost::new(
        &data_dir,
        registry.clone(),
        30_000,
        &metteur_shared::config::AddonConfig {
            require_signature: false,
            ..Default::default()
        },
    );
    let app =
        AppState::new(WorkspaceManager::new(), registry, config).with_addon_host(addon_host).await;
    let state = Arc::new(app);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_tls = match pki {
        Some(pki) => {
            // Materialize PEMs to files so the loader (and its validation) is
            // exercised.
            let dir = std::env::temp_dir().join(format!("metteur-pki-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let cert_path = dir.join("server.pem");
            let key_path = dir.join("server.key");
            let ca_path = dir.join("ca.pem");
            std::fs::write(&cert_path, &pki.server_cert).unwrap();
            std::fs::write(&key_path, &pki.server_key).unwrap();
            std::fs::write(&ca_path, &pki.ca_pem).unwrap();
            Some(metteur_daemon::tls::load(&cert_path, &key_path, &ca_path).unwrap())
        }
        None => None,
    };

    let serve_state = state.clone();
    let strict = server_tls.is_some();
    let server_tls_for_task = server_tls.clone();
    tokio::spawn(async move {
        let builder = Server::builder();
        let builder = match server_tls_for_task {
            Some(tls) => builder.tls_config(tls.server).unwrap(),
            None => builder,
        };
        builder
            .layer(AclLayer::new(state.acl_store.clone(), strict))
            .add_service(proto::daemon_server::DaemonServer::new(DaemonService::new(serve_state)))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let client = match (pki, identity) {
        (Some(pki), Some(identity)) => {
            let channel = Channel::from_shared(format!("https://{addr}"))
                .unwrap()
                .tls_config(
                    ClientTlsConfig::new()
                        .identity(Identity::from_pem(
                            identity.cert_pem.as_bytes(),
                            identity.key_pem.as_bytes(),
                        ))
                        .ca_certificate(Certificate::from_pem(pki.ca_pem.as_bytes()))
                        .domain_name("127.0.0.1"),
                )
                .unwrap()
                .connect()
                .await?;
            DaemonClient::new(channel)
        }
        // TLS server without a client identity: the handshake must fail.
        (Some(pki), None) => {
            let channel = Channel::from_shared(format!("https://{addr}"))
                .unwrap()
                .tls_config(
                    ClientTlsConfig::new()
                        .ca_certificate(Certificate::from_pem(pki.ca_pem.as_bytes()))
                        .domain_name("127.0.0.1"),
                )
                .unwrap()
                .connect()
                .await?;
            DaemonClient::new(channel)
        }
        _ => DaemonClient::connect(format!("http://{addr}")).await?,
    };
    Ok((client, workspace))
}

/// Builds a minimal blueprint: Start -> Add(A=2, B=3) -> Judge(Score=Result).
fn build_blueprint() -> proto::Blueprint {
    let start = uuid::Uuid::new_v4();
    let add = uuid::Uuid::new_v4();
    let judge = uuid::Uuid::new_v4();
    let start_exec = uuid::Uuid::new_v4();
    let start_a = uuid::Uuid::new_v4();
    let start_b = uuid::Uuid::new_v4();
    let add_exec_in = uuid::Uuid::new_v4();
    let add_exec_out = uuid::Uuid::new_v4();
    let add_a = uuid::Uuid::new_v4();
    let add_b = uuid::Uuid::new_v4();
    let add_result = uuid::Uuid::new_v4();
    let judge_exec_in = uuid::Uuid::new_v4();
    let judge_score = uuid::Uuid::new_v4();
    let judge_success = uuid::Uuid::new_v4();

    proto::Blueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "smoke".to_string(),
        nodes: vec![
            proto::Node {
                id: start.to_string(),
                node_type: "Event".to_string(),
                kind: "Start".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    proto::Pin {
                        id: start_exec.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecOutput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: start_a.to_string(),
                        name: "A".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "Float".to_string(),
                    },
                    proto::Pin {
                        id: start_b.to_string(),
                        name: "B".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "Float".to_string(),
                    },
                ],
                data_json: r#"{"A":2,"B":3}"#.to_string(),
            },
            proto::Node {
                id: add.to_string(),
                node_type: "Pure".to_string(),
                kind: "Add".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    proto::Pin {
                        id: add_exec_in.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecInput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: add_exec_out.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecOutput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: add_a.to_string(),
                        name: "A".to_string(),
                        pin_type: "DataInput".to_string(),
                        data_type: "Float".to_string(),
                    },
                    proto::Pin {
                        id: add_b.to_string(),
                        name: "B".to_string(),
                        pin_type: "DataInput".to_string(),
                        data_type: "Float".to_string(),
                    },
                    proto::Pin {
                        id: add_result.to_string(),
                        name: "Result".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "Float".to_string(),
                    },
                ],
                data_json: "{}".to_string(),
            },
            proto::Node {
                id: judge.to_string(),
                node_type: "Function".to_string(),
                kind: "Judge".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    proto::Pin {
                        id: judge_exec_in.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecInput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: judge_score.to_string(),
                        name: "Score".to_string(),
                        pin_type: "DataInput".to_string(),
                        data_type: "Float".to_string(),
                    },
                    proto::Pin {
                        id: judge_success.to_string(),
                        name: "Success".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "Bool".to_string(),
                    },
                ],
                data_json: "{}".to_string(),
            },
        ],
        edges: vec![
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_exec.to_string(),
                target_node: add.to_string(),
                target_pin: add_exec_in.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: add.to_string(),
                source_pin: add_exec_out.to_string(),
                target_node: judge.to_string(),
                target_pin: judge_exec_in.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: add.to_string(),
                source_pin: add_result.to_string(),
                target_node: judge.to_string(),
                target_pin: judge_score.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_a.to_string(),
                target_node: add.to_string(),
                target_pin: add_a.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_b.to_string(),
                target_node: add.to_string(),
                target_pin: add_b.to_string(),
            },
        ],
        entry_node_id: start.to_string(),
    }
}

/// Builds a blueprint: Start -> CallLLM(mock, delayed) -> Add.
///
/// The delayed mock LLM call gives a cancel request time to land while the
/// first node is still executing, so cancellation is observed by the next
/// node and the run is marked failed.
fn build_cancel_blueprint(delay_ms: u64) -> proto::Blueprint {
    let start = uuid::Uuid::new_v4();
    let llm = uuid::Uuid::new_v4();
    let add = uuid::Uuid::new_v4();

    let start_exec = uuid::Uuid::new_v4();
    let start_a = uuid::Uuid::new_v4();
    let start_b = uuid::Uuid::new_v4();
    let llm_exec_in = uuid::Uuid::new_v4();
    let llm_exec_out = uuid::Uuid::new_v4();
    let llm_result = uuid::Uuid::new_v4();
    let llm_context = uuid::Uuid::new_v4();
    let add_exec_in = uuid::Uuid::new_v4();
    let add_exec_out = uuid::Uuid::new_v4();
    let add_a = uuid::Uuid::new_v4();
    let add_b = uuid::Uuid::new_v4();
    let add_result = uuid::Uuid::new_v4();

    proto::Blueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "cancel".to_string(),
        nodes: vec![
            proto::Node {
                id: start.to_string(),
                node_type: "Event".to_string(),
                kind: "Start".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    proto::Pin {
                        id: start_exec.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecOutput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: start_a.to_string(),
                        name: "A".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "Float".to_string(),
                    },
                    proto::Pin {
                        id: start_b.to_string(),
                        name: "B".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "Float".to_string(),
                    },
                ],
                data_json: r#"{"A":2,"B":3}"#.to_string(),
            },
            proto::Node {
                id: llm.to_string(),
                node_type: "Function".to_string(),
                kind: "CallLLM".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    proto::Pin {
                        id: llm_exec_in.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecInput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: llm_exec_out.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecOutput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: llm_result.to_string(),
                        name: "Result".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "String".to_string(),
                    },
                    proto::Pin {
                        id: llm_context.to_string(),
                        name: "Context".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "Json".to_string(),
                    },
                ],
                data_json: format!(
                    r#"{{"provider":"mock","mock_text":"ok","mock_delay_ms":{delay_ms}}}"#
                ),
            },
            proto::Node {
                id: add.to_string(),
                node_type: "Pure".to_string(),
                kind: "Add".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    proto::Pin {
                        id: add_exec_in.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecInput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: add_exec_out.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecOutput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: add_a.to_string(),
                        name: "A".to_string(),
                        pin_type: "DataInput".to_string(),
                        data_type: "Float".to_string(),
                    },
                    proto::Pin {
                        id: add_b.to_string(),
                        name: "B".to_string(),
                        pin_type: "DataInput".to_string(),
                        data_type: "Float".to_string(),
                    },
                    proto::Pin {
                        id: add_result.to_string(),
                        name: "Result".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "Float".to_string(),
                    },
                ],
                data_json: "{}".to_string(),
            },
        ],
        edges: vec![
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_exec.to_string(),
                target_node: llm.to_string(),
                target_pin: llm_exec_in.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: llm.to_string(),
                source_pin: llm_exec_out.to_string(),
                target_node: add.to_string(),
                target_pin: add_exec_in.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_a.to_string(),
                target_node: add.to_string(),
                target_pin: add_a.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_b.to_string(),
                target_node: add.to_string(),
                target_pin: add_b.to_string(),
            },
        ],
        entry_node_id: start.to_string(),
    }
}

/// Polls `ListExecutions` until a run with the wanted status appears.
async fn wait_for_status(client: &mut DaemonClient<Channel>, ws: &str, want: &str) {
    for _ in 0..100 {
        let list = client
            .list_executions(ListExecutionsRequest {
                workspace_path: ws.to_string(),
            })
            .await
            .unwrap()
            .into_inner();
        if let Some(execution) = list.executions.first()
            && execution.status == want
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for run status {want}");
}

#[tokio::test]
async fn smoke_open_save_execute() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();

    // Open the workspace.
    let open = client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(open.path, ws_path);

    // Save a blueprint.
    let blueprint = build_blueprint();
    client
        .save_blueprint(SaveBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint: Some(blueprint.clone()),
        })
        .await
        .unwrap();

    // Execute the blueprint and collect events.
    let mut stream = client
        .execute_blueprint(ExecuteBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint_id: blueprint.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();

    let mut events = Vec::new();
    while let Some(event) = stream.message().await.unwrap() {
        events.push(event);
    }
    // Start, Add and Judge each emit started + finished + node_data.
    assert_eq!(events.len(), 9);

    // Create a snapshot.
    let snap = client
        .create_snapshot(CreateSnapshotRequest {
            workspace_path: ws_path,
            description: "smoke".to_string(),
            alias: "smoke-alias".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(snap.description, "smoke");
    assert_eq!(snap.alias, "smoke-alias");
}

#[tokio::test]
async fn smoke_completed_run_cannot_continue() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();

    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();
    let blueprint = build_blueprint();
    client
        .save_blueprint(SaveBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint: Some(blueprint.clone()),
        })
        .await
        .unwrap();

    let mut stream = client
        .execute_blueprint(ExecuteBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint_id: blueprint.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    while let Some(event) = stream.message().await.unwrap() {
        let _ = event;
    }

    // The successful run is listed as Completed.
    let list = client
        .list_executions(ListExecutionsRequest {
            workspace_path: ws_path.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.executions.len(), 1);
    assert_eq!(list.executions[0].status, "Completed");

    // Continuing a completed run is rejected.
    let err = client
        .continue_execution(ContinueExecutionRequest {
            workspace_path: ws_path,
            run_id: list.executions[0].run_id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

#[tokio::test]
async fn smoke_cancel_marks_run_failed() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();

    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();
    let blueprint = build_cancel_blueprint(300);
    client
        .save_blueprint(SaveBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint: Some(blueprint.clone()),
        })
        .await
        .unwrap();

    let mut stream = client
        .execute_blueprint(ExecuteBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint_id: blueprint.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();

    // Let the run start, then cancel it while the mock LLM call is pending.
    tokio::time::sleep(Duration::from_millis(50)).await;
    client
        .cancel_execution(CancelRequest {
            workspace_path: ws_path.clone(),
        })
        .await
        .unwrap();

    // Cancellation aborts the stream with a status; drain until it ends.
    while matches!(stream.message().await, Ok(Some(_))) {}

    wait_for_status(&mut client, &ws_path, "Failed").await;
}

#[tokio::test]
async fn smoke_get_file_history() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();

    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();

    // Write, snapshot, modify, snapshot.
    std::fs::write(workspace.join("a.txt"), "v1").unwrap();
    client
        .create_snapshot(CreateSnapshotRequest {
            workspace_path: ws_path.clone(),
            description: "added".to_string(),
            alias: "first".to_string(),
        })
        .await
        .unwrap();
    std::fs::write(workspace.join("a.txt"), "a longer version").unwrap();
    client
        .create_snapshot(CreateSnapshotRequest {
            workspace_path: ws_path.clone(),
            description: "modified".to_string(),
            alias: "second".to_string(),
        })
        .await
        .unwrap();

    let history = client
        .get_file_history(GetFileHistoryRequest {
            workspace_path: ws_path.clone(),
            path: "a.txt".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    let statuses: Vec<&str> = history.entries.iter().map(|e| e.status.as_str()).collect();
    assert_eq!(statuses, vec!["Added", "Modified"]);
    assert_eq!(history.entries.len(), 2);

    // Snapshots are stored in the version manager.
    let snapshots = client
        .list_snapshots(ListSnapshotsRequest {
            workspace_path: ws_path,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(snapshots.snapshots.len(), 2);
}

#[tokio::test]
async fn smoke_audit_log() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();

    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();
    let blueprint = build_blueprint();
    client
        .save_blueprint(SaveBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint: Some(blueprint.clone()),
        })
        .await
        .unwrap();

    let mut stream = client
        .execute_blueprint(ExecuteBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint_id: blueprint.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    while let Some(event) = stream.message().await.unwrap() {
        let _ = event;
    }

    let audit = client
        .list_audit_log(ListAuditLogRequest {
            workspace_path: ws_path,
        })
        .await
        .unwrap()
        .into_inner();
    let operations: Vec<&str> = audit.entries.iter().map(|e| e.operation.as_str()).collect();
    assert!(operations.contains(&"execution.start"));
    assert!(operations.contains(&"node.started"));
    assert!(operations.contains(&"node.finished"));
}

#[tokio::test]
async fn smoke_workspace_config_reload() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();

    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();

    let config = serde_json::json!({
        "llm": { "default_model": "test-model", "temperature": 0.5 }
    })
    .to_string();
    client
        .set_config(SetConfigRequest {
            workspace_path: ws_path.clone(),
            config_json: config,
        })
        .await
        .unwrap();

    let loaded = client
        .get_config(GetConfigRequest {
            workspace_path: ws_path.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    let parsed: serde_json::Value = serde_json::from_str(&loaded.config_json).unwrap();
    assert_eq!(parsed["llm"]["default_model"], "test-model");
    assert_eq!(parsed["llm"]["temperature"], 0.5);
}

#[tokio::test]
async fn smoke_global_audit_not_enabled() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();
    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path,
        })
        .await
        .unwrap();

    // The global audit is only enabled with a global database; without one,
    // querying it returns NotFound.
    let err = client
        .list_audit_log(ListAuditLogRequest {
            workspace_path: String::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn smoke_mtls_acls() {
    let pki = generate_pki();
    // Rules keyed by certificate subject: client1 may only list workspaces.
    let config = metteur_shared::config::Config {
        acl: metteur_shared::config::AclConfig {
            rules: vec![AclRule {
                subject: "client1".to_string(),
                allow: vec![
                    "/metteur.Daemon/ListWorkspaces".to_string(),
                    "/metteur.Daemon/OpenWorkspace".to_string(),
                ],
                deny: vec![],
            }],
        },
        ..Default::default()
    };

    // client1 is authorized.
    let (mut client1, workspace) =
        start_mtls_server(config.clone(), &pki, &pki.client1).await.expect("client1 connects");
    let ws_path = workspace.to_string_lossy().to_string();
    client1
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();
    client1.list_workspaces(proto::Empty {}).await.unwrap();

    // client2 is authenticated but not covered by the rules: denied.
    let (mut client2, _) =
        start_mtls_server(config.clone(), &pki, &pki.client2).await.expect("client2 connects");
    let err = client2.list_workspaces(proto::Empty {}).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied);

    // A client without an identity is not authenticated and cannot call:
    // the server either rejects it at the TLS layer or the ACL layer.
    let (mut anonymous, _) =
        start_server_inner(metteur_shared::config::Config::default(), Some(&pki), None)
            .await
            .expect("tls handshake");
    let err = anonymous.list_workspaces(proto::Empty {}).await.unwrap_err();
    assert_ne!(err.code(), tonic::Code::Ok);
}

/// A client signed by an unrelated CA must be rejected at the TLS layer.
///
/// Both PKIs are generated by the same tool with the default CA name, so their
/// CA subjects/issuers collide — the worst case for trust-anchor selection.
#[tokio::test]
async fn smoke_mtls_rejects_foreign_ca() {
    // Two independent PKIs whose CA subjects collide ("metteur test ca").
    let trusted = generate_pki();
    let rogue = generate_pki();

    // Server trusts only `trusted.ca`.
    let server = metteur_daemon::tls::load(
        &write_temp("server.pem", &trusted.server_cert),
        &write_temp("server.key", &trusted.server_key),
        &write_temp("ca.pem", &trusted.ca_pem),
    )
    .unwrap();

    let workspace = std::env::temp_dir().join(format!("metteur-foreign-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace).unwrap();
    let registry = Arc::new(Registry::with_builtins());
    let data_dir = workspace.join(".metteur-data");
    let addon_host = metteur_daemon::addon::AddonHost::new(
        &data_dir,
        registry.clone(),
        30_000,
        &metteur_shared::config::AddonConfig {
            require_signature: false,
            ..Default::default()
        },
    );
    let app = AppState::new(
        WorkspaceManager::new(),
        registry,
        metteur_shared::config::Config::default(),
    )
    .with_addon_host(addon_host)
    .await;
    let state = Arc::new(app);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server_config = server.server;
    let serve_state = state.clone();
    tokio::spawn(async move {
        Server::builder()
            .tls_config(server_config)
            .unwrap()
            .add_service(proto::daemon_server::DaemonServer::new(DaemonService::new(serve_state)))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    // Present the rogue identity while trusting the real CA. The server must
    // reject the certificate: issue a real RPC so a lazy handshake is forced.
    let channel = Channel::from_shared(format!("https://{addr}"))
        .unwrap()
        .tls_config(
            ClientTlsConfig::new()
                .identity(Identity::from_pem(
                    rogue.client1.cert_pem.as_bytes(),
                    rogue.client1.key_pem.as_bytes(),
                ))
                .ca_certificate(Certificate::from_pem(trusted.ca_pem.as_bytes()))
                .domain_name("127.0.0.1"),
        )
        .unwrap()
        .connect()
        .await
        .expect("tls channel");
    let mut client = DaemonClient::new(channel);
    let res = client.list_workspaces(proto::Empty {}).await;
    assert!(
        res.is_err(),
        "server accepted a client certificate signed by an untrusted CA"
    );
}

/// Writes `content` to a temp file and returns its path.
fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("metteur-foreign-pki-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// Builds a blueprint Start -> Tool(ExecuteCommand) with a `command` input.
fn build_command_blueprint(command: &str) -> proto::Blueprint {
    let start = uuid::Uuid::new_v4();
    let start_exec = uuid::Uuid::new_v4();
    let start_command = uuid::Uuid::new_v4();
    let tool = uuid::Uuid::new_v4();
    let tool_exec_in = uuid::Uuid::new_v4();
    let tool_exec_out = uuid::Uuid::new_v4();
    let tool_command_in = uuid::Uuid::new_v4();
    let tool_result = uuid::Uuid::new_v4();

    proto::Blueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "command".to_string(),
        nodes: vec![
            proto::Node {
                id: start.to_string(),
                node_type: "Event".to_string(),
                kind: "Start".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    proto::Pin {
                        id: start_exec.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecOutput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: start_command.to_string(),
                        name: "command".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "String".to_string(),
                    },
                ],
                data_json: format!(r#"{{"command":"{command}"}}"#),
            },
            proto::Node {
                id: tool.to_string(),
                node_type: "Function".to_string(),
                kind: "Tool".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    proto::Pin {
                        id: tool_exec_in.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecInput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: tool_exec_out.to_string(),
                        name: "Exec".to_string(),
                        pin_type: "ExecOutput".to_string(),
                        data_type: "Void".to_string(),
                    },
                    proto::Pin {
                        id: tool_command_in.to_string(),
                        name: "command".to_string(),
                        pin_type: "DataInput".to_string(),
                        data_type: "String".to_string(),
                    },
                    proto::Pin {
                        id: tool_result.to_string(),
                        name: "Result".to_string(),
                        pin_type: "DataOutput".to_string(),
                        data_type: "String".to_string(),
                    },
                ],
                data_json: r#"{"tool_name":"ExecuteCommand"}"#.to_string(),
            },
        ],
        edges: vec![
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_exec.to_string(),
                target_node: tool.to_string(),
                target_pin: tool_exec_in.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_command.to_string(),
                target_node: tool.to_string(),
                target_pin: tool_command_in.to_string(),
            },
        ],
        entry_node_id: start.to_string(),
    }
}

/// Opens the workspace and saves the blueprint, returning its id.
async fn prepare(
    client: &mut DaemonClient<Channel>,
    ws_path: &str,
    bp: proto::Blueprint,
) -> String {
    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.to_string(),
        })
        .await
        .unwrap();
    client
        .save_blueprint(SaveBlueprintRequest {
            workspace_path: ws_path.to_string(),
            blueprint: Some(bp.clone()),
        })
        .await
        .unwrap();
    bp.id
}

#[tokio::test]
async fn smoke_sandbox_approval_allow_once() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();

    let blueprint = build_command_blueprint("echo hello");
    let id = prepare(&mut client, &ws_path, blueprint).await;
    client
        .set_config(SetConfigRequest {
            workspace_path: ws_path.clone(),
            config_json: r#"{"sandbox":{"enabled":true}}"#.to_string(),
        })
        .await
        .unwrap();

    let mut stream = client
        .execute_blueprint(ExecuteBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint_id: id,
        })
        .await
        .unwrap()
        .into_inner();

    // Wait for the live approval request before answering it.
    let mut request_id = None;
    while let Some(event) = stream.message().await.unwrap() {
        if event.kind == "approval_request" {
            request_id = Some(event.message.clone());
            break;
        }
    }
    let request_id = request_id.expect("approval request arrives");

    client
        .respond_approval(proto::ApprovalDecisionRequest {
            workspace_path: ws_path.clone(),
            request_id,
            decision: "AllowOnce".to_string(),
        })
        .await
        .unwrap();

    while let Some(event) = stream.message().await.unwrap() {
        let _ = event;
    }

    // The run completed and the audit trail records the approval.
    let runs = client
        .list_executions(ListExecutionsRequest {
            workspace_path: ws_path.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(runs.executions[0].status, "Completed");

    let audit = client
        .list_audit_log(ListAuditLogRequest {
            workspace_path: ws_path,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(
        audit
            .entries
            .iter()
            .any(|e| e.operation == "sandbox.approval" && e.detail_json.contains("\"allowed\""))
    );
}

#[tokio::test]
async fn smoke_sandbox_approval_deny_once_fails_run() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();

    let blueprint = build_command_blueprint("echo hello");
    let id = prepare(&mut client, &ws_path, blueprint).await;
    client
        .set_config(SetConfigRequest {
            workspace_path: ws_path.clone(),
            config_json: r#"{"sandbox":{"enabled":true}}"#.to_string(),
        })
        .await
        .unwrap();

    let mut stream = client
        .execute_blueprint(ExecuteBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint_id: id,
        })
        .await
        .unwrap()
        .into_inner();

    let mut request_id = None;
    while let Some(event) = stream.message().await.unwrap() {
        if event.kind == "approval_request" {
            request_id = Some(event.message.clone());
            break;
        }
    }

    client
        .respond_approval(proto::ApprovalDecisionRequest {
            workspace_path: ws_path.clone(),
            request_id: request_id.expect("approval request arrives"),
            decision: "DenyOnce".to_string(),
        })
        .await
        .unwrap();

    // The denied command surfaces as a FailedPrecondition status.
    let mut code = tonic::Code::Ok;
    loop {
        match stream.message().await {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(status) => {
                code = status.code();
                break;
            }
        }
    }
    assert_eq!(code, tonic::Code::FailedPrecondition);

    let runs = client
        .list_executions(ListExecutionsRequest {
            workspace_path: ws_path,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(runs.executions[0].status, "Failed");
}

/// Builds a nested blueprint executed by the Abstract node:
/// Start -> Tool(WriteFile), writing constant content to a relative path.
fn build_nested_blueprint_json() -> String {
    let start = uuid::Uuid::new_v4();
    let tool = uuid::Uuid::new_v4();
    let start_exec = uuid::Uuid::new_v4();
    let start_path = uuid::Uuid::new_v4();
    let start_content = uuid::Uuid::new_v4();
    let tool_exec_in = uuid::Uuid::new_v4();
    let tool_path = uuid::Uuid::new_v4();
    let tool_content = uuid::Uuid::new_v4();
    let tool_result = uuid::Uuid::new_v4();

    serde_json::json!({
        "id": uuid::Uuid::new_v4(),
        "name": "nested",
        "entry_node_id": start,
        "nodes": [
            {
                "id": start, "node_type": "Event", "kind": "Start",
                "position": [0.0, 0.0],
                "pins": [
                    {"id": start_exec, "name": "Exec", "pin_type": "ExecOutput", "data_type": "Void"},
                    {"id": start_path, "name": "path", "pin_type": "DataOutput", "data_type": "String"},
                    {"id": start_content, "name": "content", "pin_type": "DataOutput", "data_type": "String"}
                ],
                "data": {"path": "abstract-smoke.txt", "content": "nested-ok"}
            },
            {
                "id": tool, "node_type": "Function", "kind": "Tool",
                "position": [10.0, 0.0],
                "pins": [
                    {"id": tool_exec_in, "name": "Exec", "pin_type": "ExecInput", "data_type": "Void"},
                    {"id": tool_path, "name": "path", "pin_type": "DataInput", "data_type": "String"},
                    {"id": tool_content, "name": "content", "pin_type": "DataInput", "data_type": "String"},
                    {"id": tool_result, "name": "Result", "pin_type": "DataOutput", "data_type": "String"}
                ],
                "data": {"tool_name": "WriteFile"}
            }
        ],
        "edges": [
            {"id": uuid::Uuid::new_v4(), "source_node": start, "source_pin": start_exec,
             "target_node": tool, "target_pin": tool_exec_in},
            {"id": uuid::Uuid::new_v4(), "source_node": start, "source_pin": start_path,
             "target_node": tool, "target_pin": tool_path},
            {"id": uuid::Uuid::new_v4(), "source_node": start, "source_pin": start_content,
             "target_node": tool, "target_pin": tool_content}
        ]
    })
    .to_string()
}

#[tokio::test]
async fn smoke_abstract_node_expands_and_runs() {
    use metteur_shared::config::Config;

    let (mut client, workspace) = start_server(Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();
    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();

    // The registry must expose the Abstract kind.
    let kinds = client.list_node_kinds(proto::Empty {}).await.unwrap().into_inner();
    assert!(kinds.kinds.iter().any(|k| k == "Abstract"));

    // Outer blueprint: Start -> Abstract(mock-planned sub-blueprint).
    let start = uuid::Uuid::new_v4();
    let abstract_id = uuid::Uuid::new_v4();
    let start_exec = uuid::Uuid::new_v4();
    let abstract_exec_in = uuid::Uuid::new_v4();
    let blueprint = proto::Blueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "abstract-smoke".to_string(),
        nodes: vec![
            proto::Node {
                id: start.to_string(),
                node_type: "Event".to_string(),
                kind: "Start".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![proto::Pin {
                    id: start_exec.to_string(),
                    name: "Exec".to_string(),
                    pin_type: "ExecOutput".to_string(),
                    data_type: "Void".to_string(),
                }],
                data_json: "{}".to_string(),
            },
            proto::Node {
                id: abstract_id.to_string(),
                node_type: "Function".to_string(),
                kind: "Abstract".to_string(),
                pos_x: 10.0,
                pos_y: 0.0,
                pins: vec![proto::Pin {
                    id: abstract_exec_in.to_string(),
                    name: "Exec".to_string(),
                    pin_type: "ExecInput".to_string(),
                    data_type: "Void".to_string(),
                }],
                data_json: serde_json::json!({
                    "description": "write a file",
                    "provider": "mock",
                    "mock_text": build_nested_blueprint_json(),
                })
                .to_string(),
            },
        ],
        edges: vec![proto::Edge {
            id: uuid::Uuid::new_v4().to_string(),
            source_node: start.to_string(),
            source_pin: start_exec.to_string(),
            target_node: abstract_id.to_string(),
            target_pin: abstract_exec_in.to_string(),
        }],
        entry_node_id: start.to_string(),
    };

    client
        .save_blueprint(SaveBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint: Some(blueprint.clone()),
        })
        .await
        .unwrap();

    let mut stream = client
        .execute_blueprint(ExecuteBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint_id: blueprint.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();

    let mut messages = Vec::new();
    while let Some(event) = stream.message().await.unwrap() {
        if event.kind == "message" && event.node_id == abstract_id.to_string() {
            messages.push(event.message);
        }
    }

    // The nested blueprint ran inside the abstract node.
    assert!(
        messages.iter().any(|m| m.contains("abstract expanded 2 nodes")),
        "messages: {messages:?}"
    );
    let written = std::fs::read_to_string(workspace.join("abstract-smoke.txt")).unwrap();
    assert_eq!(written, "nested-ok");
}

/// Builds a minimal addon package (manifest + WAT plugin) in `dir`.
fn build_addon_package(dir: &Path, id: &str, tool_name: &str, function: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "\
id = \"{id}\"
version = \"0.1.0\"
name = \"Smoke Addon\"

[permissions]
required = [\"tools\"]

[addon]
entry = \"main.wasm\"

[[tools]]
name = \"{tool_name}\"
function = \"{function}\"
description = \"Uppercases its input.\"
[tools.parameters]
type = \"object\"
"
        ),
    )
    .unwrap();
    let wat = format!(
        r#"(module
            (import "extism:host/user" "log" (func $log (param i64 i64)))
            (func (export "{function}") (param i64) (result i64)
                local.get 0))
        "#
    );
    let wasm = wat::parse_str(&wat).unwrap();
    std::fs::write(dir.join("main.wasm"), wasm).unwrap();
}

#[tokio::test]
async fn smoke_addon_install_call_uninstall() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();
    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();

    // Build and zip a package with an identity-transform tool.
    let pkg_dir = workspace.join("pkg");
    build_addon_package(&pkg_dir, "com.smoke.addon", "Shout", "shout");
    let zip_path = workspace.join("com.smoke.addon-0.1.0.zip");
    {
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive.start_file("manifest.toml", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(
            &mut archive,
            &std::fs::read(pkg_dir.join("manifest.toml")).unwrap(),
        )
        .unwrap();
        archive.start_file("main.wasm", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut archive, &std::fs::read(pkg_dir.join("main.wasm")).unwrap())
            .unwrap();
        archive.finish().unwrap();
    }

    let info = client
        .install_addon(proto::InstallAddonRequest {
            package_path: zip_path.to_string_lossy().to_string(),
            workspace_path: String::new(),
            granted_permissions: vec!["tools".to_string()],
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.id, "com.smoke.addon");
    assert_eq!(info.tool_count, 1);

    // The addon tool is registered under AddonIdPascal+ToolPascal.
    let tools = client.list_tools(proto::Empty {}).await.unwrap().into_inner();
    assert!(tools.tools.iter().any(|t| t.name == "ComSmokeAddonShout"));

    // Call it through a blueprint Tool node.
    let start = uuid::Uuid::new_v4();
    let tool_node = uuid::Uuid::new_v4();
    let start_exec = uuid::Uuid::new_v4();
    let text_pin = uuid::Uuid::new_v4();
    let exec_in = uuid::Uuid::new_v4();
    let result_pin = uuid::Uuid::new_v4();
    let blueprint = proto::Blueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "addon-call".to_string(),
        nodes: vec![
            proto::Node {
                id: start.to_string(),
                node_type: "Event".into(),
                kind: "Start".into(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    proto::Pin {
                        id: start_exec.to_string(),
                        name: "Exec".into(),
                        pin_type: "ExecOutput".into(),
                        data_type: "Void".into(),
                    },
                    proto::Pin {
                        id: text_pin.to_string(),
                        name: "text".into(),
                        pin_type: "DataOutput".into(),
                        data_type: "String".into(),
                    },
                ],
                data_json: r#"{"text":"make me loud"}"#.into(),
            },
            proto::Node {
                id: tool_node.to_string(),
                node_type: "Function".into(),
                kind: "Tool".into(),
                pos_x: 10.0,
                pos_y: 0.0,
                pins: vec![
                    proto::Pin {
                        id: exec_in.to_string(),
                        name: "Exec".into(),
                        pin_type: "ExecInput".into(),
                        data_type: "Void".into(),
                    },
                    proto::Pin {
                        id: text_pin.to_string(),
                        name: "text".into(),
                        pin_type: "DataInput".into(),
                        data_type: "String".into(),
                    },
                    proto::Pin {
                        id: result_pin.to_string(),
                        name: "Result".into(),
                        pin_type: "DataOutput".into(),
                        data_type: "String".into(),
                    },
                ],
                data_json: r#"{"tool_name":"ComSmokeAddonShout"}"#.into(),
            },
        ],
        edges: vec![
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_exec.to_string(),
                target_node: tool_node.to_string(),
                target_pin: exec_in.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: text_pin.to_string(),
                target_node: tool_node.to_string(),
                target_pin: text_pin.to_string(),
            },
        ],
        entry_node_id: start.to_string(),
    };
    client
        .save_blueprint(SaveBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint: Some(blueprint.clone()),
        })
        .await
        .unwrap();
    let mut stream = client
        .execute_blueprint(ExecuteBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint_id: blueprint.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    while stream.message().await.transpose().is_some() {}

    // Uninstall removes the tool from the registry.
    client
        .uninstall_addon(proto::UninstallAddonRequest {
            id: "com.smoke.addon".to_string(),
            workspace_path: String::new(),
        })
        .await
        .unwrap();
    let tools = client.list_tools(proto::Empty {}).await.unwrap().into_inner();
    assert!(!tools.tools.iter().any(|t| t.name == "ComSmokeAddonShout"));
}

#[tokio::test]
async fn smoke_file_roundtrip_and_containment() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();
    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();

    // Write, stat, read back.
    client
        .write_file(WriteFileRequest {
            workspace_path: ws_path.clone(),
            path: "src/app.ts".to_string(),
            content: "export const x = 1;\n".to_string(),
        })
        .await
        .unwrap();
    let info = client
        .stat_file(StatFileRequest {
            workspace_path: ws_path.clone(),
            path: "src/app.ts".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!info.is_dir);
    let read = client
        .read_file(ReadFileRequest {
            workspace_path: ws_path.clone(),
            path: "src/app.ts".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read.content, "export const x = 1;\n");

    // Create a dir, rename the file, list the dir, then remove everything.
    client
        .create_dir(CreateDirRequest {
            workspace_path: ws_path.clone(),
            path: "docs".to_string(),
        })
        .await
        .unwrap();
    client
        .rename_file(RenameFileRequest {
            workspace_path: ws_path.clone(),
            from: "src/app.ts".to_string(),
            to: "src/main.ts".to_string(),
        })
        .await
        .unwrap();
    let listed = client
        .list_files(ListFilesRequest {
            workspace_path: ws_path.clone(),
            dir: "src".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.entries.len(), 1);
    assert_eq!(listed.entries[0].name, "main.ts");
    client
        .remove_file(RemoveFileRequest {
            workspace_path: ws_path.clone(),
            path: "src/main.ts".to_string(),
        })
        .await
        .unwrap();
    client
        .remove_file(RemoveFileRequest {
            workspace_path: ws_path.clone(),
            path: "docs".to_string(),
        })
        .await
        .unwrap();

    // The root listing hides the .metteur metadata directory (the harness's
    // separate `.metteur-data` dir may remain).
    let root = client
        .list_files(ListFilesRequest {
            workspace_path: ws_path.clone(),
            dir: String::new(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(root.entries.iter().all(|e| e.name != ".metteur"));

    // Escapes and metadata writes are rejected.
    let err = client
        .write_file(WriteFileRequest {
            workspace_path: ws_path.clone(),
            path: "../evil.txt".to_string(),
            content: "x".to_string(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    let err = client
        .write_file(WriteFileRequest {
            workspace_path: ws_path,
            path: ".metteur/db".to_string(),
            content: "x".to_string(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn smoke_chat_streams_mock_reply() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();
    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();

    let mut stream = client
        .send_chat(SendChatRequest {
            workspace_path: ws_path.clone(),
            message: "hello".to_string(),
            history_json: String::new(),
            options_json: r#"{"provider":"mock","mock_text":"hi there","mock_delay_ms":30}"#
                .to_string(),
        })
        .await
        .unwrap()
        .into_inner();

    let mut kinds = Vec::new();
    while let Some(event) = stream.message().await.unwrap() {
        kinds.push(event.kind.clone());
        if event.kind == "assistant" {
            assert_eq!(event.content, "hi there");
        }
    }
    assert!(kinds.contains(&"assistant".to_string()));
    assert!(kinds.contains(&"done".to_string()));
}

#[tokio::test]
async fn smoke_chat_single_slot_per_workspace() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();
    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();

    let _first = client
        .send_chat(SendChatRequest {
            workspace_path: ws_path.clone(),
            message: "slow".to_string(),
            history_json: String::new(),
            options_json: r#"{"provider":"mock","mock_text":"slow","mock_delay_ms":300}"#
                .to_string(),
        })
        .await
        .unwrap()
        .into_inner();

    // A second chat while the first is streaming is rejected.
    let err = client
        .send_chat(SendChatRequest {
            workspace_path: ws_path.clone(),
            message: "again".to_string(),
            history_json: String::new(),
            options_json: r#"{"provider":"mock","mock_text":"x"}"#.to_string(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);

    // Drain the first chat so the slot is released.
    client
        .abort_chat(AbortChatRequest {
            workspace_path: ws_path,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn smoke_chat_abort_interrupts_and_releases_slot() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();
    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();

    let mut stream = client
        .send_chat(SendChatRequest {
            workspace_path: ws_path.clone(),
            message: "hi".to_string(),
            history_json: String::new(),
            options_json: r#"{"provider":"mock","mock_text":"x","mock_delay_ms":1000}"#
                .to_string(),
        })
        .await
        .unwrap()
        .into_inner();

    tokio::time::sleep(Duration::from_millis(100)).await;
    client
        .abort_chat(AbortChatRequest {
            workspace_path: ws_path.clone(),
        })
        .await
        .unwrap();

    // The stream ends (with an error or cleanly); the slot is then released.
    loop {
        match stream.message().await {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    client
        .send_chat(SendChatRequest {
            workspace_path: ws_path,
            message: "after".to_string(),
            history_json: String::new(),
            options_json: r#"{"provider":"mock","mock_text":"ok"}"#.to_string(),
        })
        .await
        .unwrap();
}

/// Builds a `SmokeAdd` function body: FunctionEntry(A, B) -> Add -> FunctionExit.
fn smoke_add_function() -> proto::Blueprint {
    let pin = |id: uuid::Uuid, name: &str, pin_type: &str, data_type: &str| proto::Pin {
        id: id.to_string(),
        name: name.to_string(),
        pin_type: pin_type.to_string(),
        data_type: data_type.to_string(),
    };
    let (fn_id, entry, add, exit) =
        (uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let (entry_ex, entry_a, entry_b, add_exin, add_exout, add_a, add_b, add_res, exit_ex, exit_res) = (
        uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(), uuid::Uuid::new_v4(),
    );
    proto::Blueprint {
        id: fn_id.to_string(),
        name: "SmokeAdd".to_string(),
        nodes: vec![
            proto::Node {
                id: entry.to_string(),
                node_type: "Event".to_string(),
                kind: "FunctionEntry".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    pin(entry_ex, "Exec", "ExecOutput", "Void"),
                    pin(entry_a, "A", "DataOutput", "Float"),
                    pin(entry_b, "B", "DataOutput", "Float"),
                ],
                data_json: "{}".to_string(),
            },
            proto::Node {
                id: add.to_string(),
                node_type: "Pure".to_string(),
                kind: "Add".to_string(),
                pos_x: 120.0,
                pos_y: 0.0,
                pins: vec![
                    pin(add_exin, "Exec", "ExecInput", "Void"),
                    pin(add_exout, "Exec", "ExecOutput", "Void"),
                    pin(add_a, "A", "DataInput", "Float"),
                    pin(add_b, "B", "DataInput", "Float"),
                    pin(add_res, "Result", "DataOutput", "Float"),
                ],
                data_json: "{}".to_string(),
            },
            proto::Node {
                id: exit.to_string(),
                node_type: "Event".to_string(),
                kind: "FunctionExit".to_string(),
                pos_x: 260.0,
                pos_y: 0.0,
                pins: vec![
                    pin(exit_ex, "Exec", "ExecInput", "Void"),
                    pin(exit_res, "Result", "DataInput", "Float"),
                ],
                data_json: "{}".to_string(),
            },
        ],
        edges: vec![
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: entry.to_string(),
                source_pin: entry_ex.to_string(),
                target_node: add.to_string(),
                target_pin: add_exin.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: add.to_string(),
                source_pin: add_exout.to_string(),
                target_node: exit.to_string(),
                target_pin: exit_ex.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: entry.to_string(),
                source_pin: entry_a.to_string(),
                target_node: add.to_string(),
                target_pin: add_a.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: entry.to_string(),
                source_pin: entry_b.to_string(),
                target_node: add.to_string(),
                target_pin: add_b.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: add.to_string(),
                source_pin: add_res.to_string(),
                target_node: exit.to_string(),
                target_pin: exit_res.to_string(),
            },
        ],
        entry_node_id: entry.to_string(),
    }
}

#[tokio::test]
async fn smoke_function_library_save_execute() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();
    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();

    // Save a workspace-scoped function.
    let body = smoke_add_function();
    let saved = client
        .save_function(SaveFunctionRequest {
            workspace_path: ws_path.clone(),
            info: Some(FunctionInfo {
                id: uuid::Uuid::new_v4().to_string(),
                name: "SmokeAdd".to_string(),
                description: "adds numbers".to_string(),
                inputs: vec![
                    FnPin { name: "A".to_string(), data_type: "Float".to_string(), description: String::new() },
                    FnPin { name: "B".to_string(), data_type: "Float".to_string(), description: String::new() },
                ],
                outputs: vec![FnPin { name: "Result".to_string(), data_type: "Float".to_string(), description: String::new() }],
                source: String::new(),
                updated_at: 0,
            }),
            body: Some(body),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(saved.info.unwrap().name, "SmokeAdd");

    // The library lists the saved function and the builtin ChainOfThought.
    let list = client
        .list_functions(ListFunctionsRequest {
            workspace_path: ws_path.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    let names: Vec<String> = list.functions.iter().map(|f| f.name.clone()).collect();
    assert!(names.contains(&"SmokeAdd".to_string()));
    assert!(names.contains(&"ChainOfThought".to_string()));

    // Build a root blueprint that calls it: Start(A=5,B=3) -> CallFunction.
    let (start, caller) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let (start_ex, start_a, start_b, call_exin, call_exout, call_a, call_b, call_res) = (
        uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4(),
    );
    let pin = |id: uuid::Uuid, name: &str, pin_type: &str, data_type: &str| proto::Pin {
        id: id.to_string(),
        name: name.to_string(),
        pin_type: pin_type.to_string(),
        data_type: data_type.to_string(),
    };
    let root = proto::Blueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "call-smoke".to_string(),
        nodes: vec![
            proto::Node {
                id: start.to_string(),
                node_type: "Event".to_string(),
                kind: "Start".to_string(),
                pos_x: 0.0,
                pos_y: 0.0,
                pins: vec![
                    pin(start_ex, "Exec", "ExecOutput", "Void"),
                    pin(start_a, "A", "DataOutput", "Float"),
                    pin(start_b, "B", "DataOutput", "Float"),
                ],
                data_json: r#"{"A":5,"B":3}"#.to_string(),
            },
            proto::Node {
                id: caller.to_string(),
                node_type: "Function".to_string(),
                kind: "CallFunction".to_string(),
                pos_x: 120.0,
                pos_y: 0.0,
                pins: vec![
                    pin(call_exin, "Exec", "ExecInput", "Void"),
                    pin(call_exout, "Exec", "ExecOutput", "Void"),
                    pin(call_a, "A", "DataInput", "Float"),
                    pin(call_b, "B", "DataInput", "Float"),
                    pin(call_res, "Result", "DataOutput", "Float"),
                ],
                data_json: r#"{"function":"SmokeAdd"}"#.to_string(),
            },
        ],
        edges: vec![
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_ex.to_string(),
                target_node: caller.to_string(),
                target_pin: call_exin.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_a.to_string(),
                target_node: caller.to_string(),
                target_pin: call_a.to_string(),
            },
            proto::Edge {
                id: uuid::Uuid::new_v4().to_string(),
                source_node: start.to_string(),
                source_pin: start_b.to_string(),
                target_node: caller.to_string(),
                target_pin: call_b.to_string(),
            },
        ],
        entry_node_id: start.to_string(),
    };
    client
        .save_blueprint(SaveBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint: Some(root.clone()),
        })
        .await
        .unwrap();

    // Execute and expect the caller's node_data event to carry Result = 8.
    let mut stream = client
        .execute_blueprint(ExecuteBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint_id: root.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    let mut found = false;
    while let Some(event) = stream.message().await.unwrap() {
        if event.kind == "node_data" {
            let detail: serde_json::Value = serde_json::from_str(&event.detail_json).unwrap();
            if let Some(outputs) = detail.get("outputs").and_then(|o| o.as_object()) {
                for v in outputs.values() {
                    if v.as_f64() == Some(8.0) {
                        found = true;
                    }
                }
            }
        }
    }
    assert!(found, "caller node_data must report the function result 8");

    // Load the function body back and delete it.
    let loaded = client
        .load_function(LoadFunctionRequest {
            workspace_path: ws_path.clone(),
            name: "SmokeAdd".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(loaded.body.is_some());
    client
        .delete_function(DeleteFunctionRequest {
            workspace_path: ws_path,
            name: "SmokeAdd".to_string(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn smoke_full_pipeline() {
    let (mut client, workspace) = start_server(metteur_shared::config::Config::default()).await;
    let ws_path = workspace.to_string_lossy().to_string();

    // Open the workspace.
    client
        .open_workspace(OpenWorkspaceRequest {
            path: ws_path.clone(),
        })
        .await
        .unwrap();

    // Compile a DSL source straight into a blueprint and save it.
    let compiled = client
        .compile_dsl(CompileDslRequest {
            source: "\
blueprint \"FullFlow\"
entry start: Start(A = 4, B = 3)
sum: Add(A <- start.A, B <- start.B)
check: Judge(Score <- sum.Result)
start -> sum
sum -> check
"
            .to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(compiled.nodes.len(), 3);
    client
        .save_blueprint(SaveBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint: Some(compiled.clone()),
        })
        .await
        .unwrap();

    // Execute and drain the live event stream.
    let mut stream = client
        .execute_blueprint(ExecuteBlueprintRequest {
            workspace_path: ws_path.clone(),
            blueprint_id: compiled.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    let mut events = Vec::new();
    while let Some(event) = stream.message().await.unwrap() {
        events.push(event);
    }
    // Start, Add and Judge each emit started + finished + node_data.
    assert_eq!(events.len(), 9);

    let runs = client
        .list_executions(ListExecutionsRequest {
            workspace_path: ws_path.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(runs.executions[0].status, "Completed");
    let run_id = runs.executions[0].run_id.clone();

    // Write a file, snapshot it under an alias, mutate, then roll back.
    client
        .write_file(WriteFileRequest {
            workspace_path: ws_path.clone(),
            path: "notes.txt".to_string(),
            content: "v1".to_string(),
        })
        .await
        .unwrap();
    client
        .create_snapshot(CreateSnapshotRequest {
            workspace_path: ws_path.clone(),
            description: "pre-rollback".to_string(),
            alias: "fp-checkpoint".to_string(),
        })
        .await
        .unwrap();
    client
        .write_file(WriteFileRequest {
            workspace_path: ws_path.clone(),
            path: "notes.txt".to_string(),
            content: "v2".to_string(),
        })
        .await
        .unwrap();
    let read = client
        .read_file(ReadFileRequest {
            workspace_path: ws_path.clone(),
            path: "notes.txt".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read.content, "v2");
    client
        .rollback(RollbackRequest {
            workspace_path: ws_path.clone(),
            snapshot_id: String::new(),
            alias: "fp-checkpoint".to_string(),
        })
        .await
        .unwrap();
    let read = client
        .read_file(ReadFileRequest {
            workspace_path: ws_path.clone(),
            path: "notes.txt".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read.content, "v1");

    // The file timeline records the rollback history.
    let history = client
        .get_file_history(GetFileHistoryRequest {
            workspace_path: ws_path.clone(),
            path: "notes.txt".to_string(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(
        !history.entries.is_empty(),
        "file history should record the mutation and rollback"
    );
    for entry in &history.entries {
        assert!(
            matches!(entry.status.as_str(), "Added" | "Modified" | "Deleted" | "Unchanged"),
            "unexpected history status {}",
            entry.status
        );
    }

    // Usage, audit and cleanup.
    client
        .get_execution_usage(GetExecutionUsageRequest {
            workspace_path: ws_path.clone(),
            run_id,
        })
        .await
        .unwrap();
    let audit = client
        .list_audit_log(ListAuditLogRequest {
            workspace_path: ws_path.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(
        !audit.entries.is_empty(),
        "workspace audit must record the execution lifecycle"
    );
    client
        .close_workspace(CloseWorkspaceRequest {
            path: ws_path,
        })
        .await
        .unwrap();
}
