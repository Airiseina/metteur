//! Parses DSL source text into a flat statement list.
//!
//! The Pest grammar in [`grammar.pest`](dsl/grammar.pest) only recognises the
//! lexical shape of the language; node validation and edge resolution happen
//! later in [`compile`](super::compile). Grammar failures surface as
//! [`SharedError::Invalid`](crate::error::SharedError::Invalid) with a
//! line/column hint taken from the Pest error span.

use pest::Parser;
use pest::error::LineColLocation;
use pest::iterators::Pair;
use pest_derive::Parser as DslParserMacro;

use crate::error::{SharedError, SharedResult};

#[derive(DslParserMacro)]
#[grammar = "dsl/grammar.pest"]
struct DslParser;

/// A DSL statement waiting for compilation.
pub(crate) enum Statement {
    /// `blueprint "Name"`.
    Header {
        name: String,
    },
    /// `[entry] alias: Kind(pin = lit, pin <- alias.pin)`.
    Node {
        alias: String,
        kind: String,
        constants: Vec<(String, serde_json::Value)>,
        refs: Vec<(String, String)>,
        is_entry: bool,
        line: usize,
        col: usize,
    },
    /// `alias[.pin] -> alias`.
    ExecEdge {
        source: String,
        source_pin: Option<String>,
        target: String,
        line: usize,
        col: usize,
    },
    /// `$target.pin <- source.pin`.
    DataWire {
        target: (String, String),
        source: (String, String),
        line: usize,
        col: usize,
    },
}

/// Parses `source` into statements, reporting the first grammar error.
pub(crate) fn parse(source: &str) -> SharedResult<Vec<Statement>> {
    let mut pairs = DslParser::parse(Rule::document, source).map_err(|e| {
        let (line, col) = match e.line_col {
            LineColLocation::Pos((l, c)) => (l, c),
            LineColLocation::Span(s, _) => (s.0, s.1),
        };
        SharedError::Invalid(format!("{} (line {line}, column {col})", e.variant.message()))
    })?;
    let document = pairs.next().expect("document is the single root rule");
    document
        .into_inner()
        .filter(|p| !matches!(p.as_rule(), Rule::EOI))
        .map(statement)
        .collect::<SharedResult<Vec<_>>>()
}

/// Builds one statement from its parse pair.
fn statement(pair: Pair<'_, Rule>) -> SharedResult<Statement> {
    let (line, col) = pair.as_span().start_pos().line_col();
    match pair.as_rule() {
        Rule::header => {
            let name = pair
                .into_inner()
                .next()
                .map(|s| s.as_str())
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            Ok(Statement::Header {
                name,
            })
        }
        Rule::node | Rule::entry_node => {
            let is_entry = pair.as_rule() == Rule::entry_node;
            let mut alias = String::new();
            let mut kind = String::new();
            let mut constants = Vec::new();
            let mut refs = Vec::new();
            for child in pair.into_inner() {
                match child.as_rule() {
                    Rule::alias => alias = child.as_str().to_string(),
                    Rule::kind => kind = child.as_str().to_string(),
                    Rule::paren_args => {
                        for param in child.into_inner().flat_map(|nested| {
                            // `param_list` wraps params one level when present.
                            if nested.as_rule() == Rule::param_list {
                                nested.into_inner().collect::<Vec<_>>()
                            } else {
                                vec![nested]
                            }
                        }) {
                            let mut pin = String::new();
                            for part in param.into_inner() {
                                match part.as_rule() {
                                    Rule::pin_name => pin = part.as_str().to_string(),
                                    Rule::literal => {
                                        constants
                                            .push((pin.clone(), parse_literal(part.as_str())?));
                                    }
                                    Rule::data_ref => {
                                        refs.push((pin.clone(), part.as_str().to_string()));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    // `{ key: literal, ... }` block — each init_item is one keyed
                    // constant; values land in node data under the bare key.
                    Rule::init_block => {
                        for item in child.into_inner() {
                            let mut pin = String::new();
                            for part in item.into_inner() {
                                match part.as_rule() {
                                    Rule::pin_name => pin = part.as_str().to_string(),
                                    Rule::literal => {
                                        constants
                                            .push((pin.clone(), parse_literal(part.as_str())?));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Statement::Node {
                alias,
                kind,
                constants,
                refs,
                is_entry,
                line,
                col,
            })
        }
        Rule::exec_edge => {
            let children: Vec<Pair<'_, Rule>> = pair.into_inner().collect();
            let (source, source_pin, target) = match children.len() {
                3 => (
                    children[0].as_str().to_string(),
                    Some(children[1].as_str().to_string()),
                    children[2].as_str().to_string(),
                ),
                2 => (children[0].as_str().to_string(), None, children[1].as_str().to_string()),
                _ => {
                    return Err(SharedError::Invalid(format!(
                        "malformed exec edge (line {line}, column {col})"
                    )));
                }
            };
            Ok(Statement::ExecEdge {
                source,
                source_pin,
                target,
                line,
                col,
            })
        }
        Rule::data_wire => {
            let mut parts = pair.into_inner();
            let target = pin_ref(parts.next().expect("data_wire has a target"))?;
            let source = pin_ref(parts.next().expect("data_wire has a source"))?;
            Ok(Statement::DataWire {
                target,
                source,
                line,
                col,
            })
        }
        _ => Err(SharedError::Invalid(format!(
            "unexpected parse token {:?} (line {line}, column {col})",
            pair.as_rule()
        ))),
    }
}

/// Splits a `pin_ref` pair into `(alias, pin_name)`.
fn pin_ref(pair: Pair<'_, Rule>) -> SharedResult<(String, String)> {
    let mut parts = pair.into_inner();
    let alias = parts.next().expect("pin_ref has an alias").as_str();
    let pin = parts.next().expect("pin_ref has a pin").as_str();
    Ok((alias.to_string(), pin.to_string()))
}

/// Parses a JSON-ish literal (numbers, strings, booleans, arrays, objects).
fn parse_literal(text: &str) -> SharedResult<serde_json::Value> {
    if text.starts_with('"') {
        return serde_json::from_str(text)
            .map_err(|e| SharedError::Invalid(format!("invalid string literal: {e}")));
    }
    if text == "true" {
        return Ok(serde_json::json!(true));
    }
    if text == "false" {
        return Ok(serde_json::json!(false));
    }
    if text.starts_with('[') || text.starts_with('{') {
        return serde_json::from_str(text)
            .map_err(|e| SharedError::Invalid(format!("invalid literal: {e}")));
    }
    // The grammar accepts an optional leading `+` (e.g. `+4`); neither i64 nor
    // f64 `FromStr` parses it, and JSON forbids it, so strip it explicitly.
    let n = text.strip_prefix('+').unwrap_or(text);
    if let Ok(n) = n.parse::<i64>() {
        return Ok(serde_json::json!(n));
    }
    if let Ok(f) = n.parse::<f64>() {
        return Ok(serde_json::json!(f));
    }
    Err(SharedError::Invalid(format!("unrecognized literal '{text}'")))
}
