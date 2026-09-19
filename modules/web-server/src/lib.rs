//! Metteur Web Server Client.
//!
//! Serves the webcore build and exposes the daemon's gRPC API to browsers as
//! grpc-web — plus a server-sent-events endpoint for the chat stream, which a
//! browser can consume over plain HTTP.

pub mod cli;
pub mod proxy;
pub mod server;
pub mod sse;

pub use cli::Cli;
pub use proxy::ForwardService;
pub use server::{build_router, serve};