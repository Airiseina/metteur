//! Metteur command-line client: a REPL over the daemon gRPC API.

pub mod commands;
pub mod net;
pub mod print;
pub mod repl;
pub mod tui;

pub use net::session;
pub use net::spawn as daemon_spawn;