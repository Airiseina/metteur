//! Metteur Web Server Client.
//!
//! Serves the webcore build and exposes the daemon's gRPC API to browsers as
//! grpc-web, proxying every call to the real daemon over gRPC.

pub mod cli;
pub mod proxy;
pub mod server;

pub use cli::Cli;
pub use proxy::ForwardService;
pub use server::{build_router, serve};