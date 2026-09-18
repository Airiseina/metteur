//! A human-friendly DSL that compiles into [`Blueprint`] JSON.
//!
//! The DSL is line-oriented; `#` starts a comment and arguments may span
//! lines inside parentheses:
//!
//! ```text
//! blueprint "MyFlow"
//! entry start: Start(A = 4, B = 3)
//! sum: Add(A <- start.A, B <- start.B)
//! verify: Validator(Actual <- sum.Result, mode = "gte", Expected = 6)
//! start -> sum
//! sum -> verify
//! ```
//!
//! The syntax is defined by the Pest grammar in [`grammar.pest`](dsl/grammar.pest);
//! [`parser`] turns source text into statements, [`compile`] validates them
//! against the built-in node template table and produces the blueprint, and
//! [`decompile`] renders a blueprint back to DSL text.

mod compile;
mod decompile;
mod parser;
mod shorthand;

pub use compile::compile;
pub use decompile::decompile;
pub use shorthand::{compile_draft, compile_draft_value};