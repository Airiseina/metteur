//! Data-type compatibility and value coercion for blueprint pins.

use super::blueprint::DataType;
use super::value::Value;

/// Whether a value of `source` type may flow into a `target` pin.
///
/// The matrix is deliberately loose: `Any`/`Json` accept everything, numeric
/// types are interchangeable, and object/list types compare structurally.
/// `Context` only pairs with `Context`.
pub fn compatible(source: &DataType, target: &DataType) -> bool {
    match (source, target) {
        (_, DataType::Any) | (DataType::Any, _) => true,
        (_, DataType::Json) | (DataType::Json, _) => true,
        (DataType::Context, DataType::Context) => true,
        (DataType::Context, _) | (_, DataType::Context) => false,
        (DataType::List(a), DataType::List(b)) => compatible(a, b),
        (DataType::Object(a), DataType::Object(b)) => {
            if b.is_empty() {
                return true;
            }
            a.iter()
                .all(|(name, a_type)| b.get(name).is_none_or(|b_type| compatible(a_type, b_type)))
        }
        (DataType::Int, DataType::Float) | (DataType::Float, DataType::Int) => true,
        // A choice is a constrained string: strings flow into it freely and it
        // flows out as a string.
        (DataType::Choice, DataType::Choice)
        | (DataType::Choice, DataType::String)
        | (DataType::String, DataType::Choice) => true,
        (DataType::Int, DataType::Int)
        | (DataType::Float, DataType::Float)
        | (DataType::Bool, DataType::Bool)
        | (DataType::String, DataType::String)
        | (DataType::Void, DataType::Void) => true,
        (DataType::List(_), _) | (_, DataType::List(_)) | (DataType::Object(_), _) => false,
        _ => false,
    }
}

/// Attempts an implicit conversion of `value` into `target`.
///
/// Returns `None` when the conversion is not possible; scalar widening
/// (`Int -> Float`, numeric/`Bool`/string/JSON scalars between each other) is
/// supported so literals compose freely. Supertype targets (`Any`, `Json`)
/// pass values through unchanged.
pub fn coerce(value: &Value, target: &DataType) -> Option<Value> {
    let source = data_type_of(value);
    if source == *target {
        return Some(value.clone());
    }
    match (value, target) {
        (Value::Int(i), DataType::Float) => Some(Value::Float(*i as f64)),
        (Value::Float(f), DataType::Int) => Some(Value::Int(*f as i64)),
        (Value::Int(i), DataType::String) => Some(Value::String(i.to_string())),
        (Value::Float(f), DataType::String) => Some(Value::String(f.to_string())),
        (Value::Bool(b), DataType::String) => Some(Value::String(b.to_string())),
        (Value::String(s), DataType::Int) => s.trim().parse::<i64>().ok().map(Value::Int),
        (Value::String(s), DataType::Float) => s.trim().parse::<f64>().ok().map(Value::Float),
        (Value::String(s), DataType::Choice) => Some(Value::String(s.clone())),
        (Value::String(s), DataType::Bool) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(Value::Bool(true)),
            "false" | "0" | "no" => Some(Value::Bool(false)),
            _ => None,
        },
        (Value::Json(j), DataType::Bool) => j.as_bool().map(Value::Bool),
        (Value::Json(j), DataType::Int) => j.as_i64().map(Value::Int),
        (Value::Json(j), DataType::Float) => j.as_f64().map(Value::Float),
        (Value::Json(j), DataType::String) => Some(if let Some(s) = j.as_str() {
            Value::String(s.to_string())
        } else {
            Value::String(j.to_string())
        }),
        // Recursively coerce list elements toward the target element type.
        (Value::List(items), DataType::List(inner)) => {
            if **inner == DataType::Any {
                Some(value.clone())
            } else {
                Some(Value::List(
                    items
                        .iter()
                        .map(|item| coerce(item, inner).unwrap_or_else(|| item.clone()))
                        .collect(),
                ))
            }
        }
        // Supertype targets and nulls pass through unchanged.
        (_, DataType::Any) | (_, DataType::Json) | (Value::Null, _) => Some(value.clone()),
        _ => None,
    }
}

/// Returns the type of a runtime value.
pub fn data_type_of(value: &Value) -> DataType {
    match value {
        Value::Null => DataType::Void,
        Value::Bool(_) => DataType::Bool,
        Value::Int(_) => DataType::Int,
        Value::Float(_) => DataType::Float,
        Value::String(_) => DataType::String,
        Value::List(items) => {
            let element = items.first().map(data_type_of).unwrap_or(DataType::Any);
            DataType::List(Box::new(element))
        }
        Value::Json(j) => {
            if j.is_array() {
                DataType::List(Box::new(DataType::Any))
            } else if j.is_object() {
                DataType::Object(Default::default())
            } else {
                DataType::Json
            }
        }
        Value::Context(_) => DataType::Context,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ContextManager;

    fn dt(s: &str) -> DataType {
        s.parse().unwrap()
    }

    #[test]
    fn compatible_matrix() {
        assert!(compatible(&dt("int"), &dt("float")));
        assert!(compatible(&dt("list<int>"), &dt("list<int>")));
        assert!(!compatible(&dt("list<int>"), &dt("list<string>")));
        assert!(compatible(&dt("string"), &dt("any")));
        assert!(compatible(&dt("any"), &dt("bool")));
        assert!(compatible(&dt("context"), &dt("context")));
        assert!(!compatible(&dt("context"), &dt("string")));
        assert!(compatible(&dt("object{a:int}"), &dt("object{a:int,b:string}")));
        assert!(!compatible(&dt("object{a:int}"), &dt("object{a:string}")));
    }

    #[test]
    fn coerce_widens_scalars() {
        assert_eq!(coerce(&Value::Int(3), &dt("float")), Some(Value::Float(3.0)));
        assert_eq!(coerce(&Value::Int(3), &dt("string")), Some(Value::String("3".into())));
        assert_eq!(coerce(&Value::String("4".into()), &dt("int")), Some(Value::Int(4)));
        assert_eq!(coerce(&Value::String("nope".into()), &dt("int")), None);
        assert_eq!(coerce(&Value::Bool(true), &dt("string")), Some(Value::String("true".into())));
    }

    #[test]
    fn context_only_pairs_with_context() {
        let ctx = ContextManager::default();
        assert!(coerce(&Value::Context(ctx.clone()), &dt("context")).is_some());
        assert!(coerce(&Value::Context(ctx), &dt("string")).is_none());
    }
}