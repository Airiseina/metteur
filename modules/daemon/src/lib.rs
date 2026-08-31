//! The Metteur daemon library.
//!
//! This crate exposes the daemon's core modules so they can be reused by the
//! `metteurd` binary and exercised by integration tests.

pub mod addon;
pub mod anon;
pub mod audit;
pub mod autostart;
pub mod billing;
pub mod cert;
pub mod cli;
pub mod config;
pub mod depgraph;
pub mod error;
pub mod execution;
pub mod fs;
pub mod grpc;
pub mod llm;
pub mod lsp;
pub mod mcp;
pub mod metrics;
pub mod persistence;
pub mod process;
pub mod registry;
pub mod replan;
pub mod sandbox;
pub mod service;
pub mod signer;
pub mod startup;
pub mod tls;
pub mod versioning;
pub mod wake;
pub mod workspace;

pub use error::{DaemonError, DaemonResult};
pub use grpc::{AppState, DaemonService};
pub use registry::Registry;
pub use workspace::WorkspaceManager;
