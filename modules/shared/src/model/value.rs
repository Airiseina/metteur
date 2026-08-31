//! Data values flowing through blueprint pins.

use serde::{Deserialize, Serialize};

use crate::llm::ContextManager;

/// A runtime value carried by a data pin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    /// A null / empty value.
    Null,
    /// A boolean value.
    Bool(bool),
    /// A signed integer value.
    Int(i64),
    /// A floating point value.
    Float(f64),
    /// A UTF-8 string value.
    String(String),
    /// A list of values.
    List(Vec<Value>),
    /// An arbitrary JSON value.
    Json(serde_json::Value),
    /// An LLM context manager.
    Context(ContextManager),
}

impl Value {
    /// Returns the value as a string, if it is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the value as a boolean, if it is a boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns the value as an integer, if it is an integer.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Returns the value as a float, if it is numeric.
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// Returns the value as a context manager, if it is one.
    pub fn as_context(&self) -> Option<&ContextManager> {
        match self {
            Value::Context(ctx) => Some(ctx),
            _ => None,
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}
