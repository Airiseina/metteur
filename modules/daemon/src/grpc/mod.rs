//! gRPC service layer.

pub mod acl;
pub mod server;
pub mod service;

/// The generated protobuf and service code.
pub use metteur_proto::proto;

pub use server::serve;
pub use service::{AppState, DaemonService};
