//! System-prompt assembly for Metteur agents.
//!
//! Every model conversation starts from a set of system fragments owned by the
//! harness: the agent's identity and working rules, its verification and
//! planning discipline, the runtime environment, and the workspace's own
//! instructions. They are ordinary [`SystemFragment`]s distinguished by their
//! `harness.` scope prefix, which lets [`HarnessPrompt::refresh`] replace
//! exactly the harness-owned fragments while leaving addon and node fragments
//! alone.
//!
//! The crate is deliberately free of clocks, environment lookups and I/O
//! policy: callers pass an [`EnvFacts`] value in, so assembly is a pure
//! function that can be tested without a daemon. See `README.md` for the
//! prompt authoring standard.
//!
//! [`SystemFragment`]: metteur_shared::llm::SystemFragment

pub mod assembly;
pub mod environment;
pub mod facts;
pub mod project;
pub mod sections;

pub use assembly::{HarnessPrompt, is_harness_fragment};
pub use facts::EnvFacts;
