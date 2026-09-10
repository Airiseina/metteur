//! Renders a blueprint back to DSL text (best effort).

use uuid::Uuid;

use crate::model::blueprint::{Blueprint, Pin, PinType};

/// Renders a blueprint back to DSL text (best effort).
///
/// The output is structurally faithful: node count, exec edges and constants
/// survive a round trip through [`compile`](super::compile). Data edges are
/// emitted as standalone `$target.pin <- source.pin` lines and data references
/// inside node args are dropped to avoid duplicate edges.
pub fn decompile(blueprint: &Blueprint) -> String {
    let mut out = String::new();
    out.push_str(&format!("blueprint \"{}\"\n", blueprint.name));
    let node_of = |id: Uuid| blueprint.nodes.iter().find(|n| n.id == id).unwrap();
    let entry = node_of(blueprint.entry_node_id);
    for (i, node) in blueprint.nodes.iter().enumerate() {
        // Inline constants ride on the pin key (the same key the canvas uses
        // for node data), so literals survive a compile round trip.
        let lit = |pin: &Pin| -> Option<String> {
            let key = pin.key.as_deref().unwrap_or(&pin.name);
            node.data.get(key).and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    Some(v.to_string())
                }
            })
        };
        let args: Vec<String> = node
            .pins
            .iter()
            .filter(|p| {
                p.pin_type == PinType::DataInput
                    || (node.kind == "Start" && p.pin_type == PinType::DataOutput)
            })
            .filter_map(|p| {
                let key = p.key.as_deref().unwrap_or(&p.name);
                lit(p).map(|v| format!("{key}: {v}"))
            })
            .collect();
        // Non-pin data entries (validator mode/regex/retry and similar node
        // config) ride along as extra keyed items so they survive the round
        // trip; keys already covered by a pin are skipped.
        let mut args = args;
        // A Switch without an explicit `cases` entry derives it from its
        // `Case_*` outlets so the branch set survives the round trip.
        if node.kind == "Switch" && node.data.get("cases").is_none() {
            let cases: Vec<String> = node
                .pins
                .iter()
                .filter(|p| p.pin_type == PinType::ExecOutput)
                .filter_map(|p| p.name.strip_prefix("Case_"))
                .map(|c| format!("\"{c}\""))
                .collect();
            if !cases.is_empty() {
                // No spaces inside the array: `array_lit` is an atomic rule.
                args.push(format!("cases: [{}]", cases.join(",")));
            }
        }
        for (key, value) in node.data.as_object().into_iter().flatten() {
            if value.is_null()
                || node.pins.iter().any(|p| p.key.as_deref() == Some(key) || &p.name == key)
            {
                continue;
            }
            args.push(format!("{key}: {value}"));
        }
        let head = if node.id == entry.id {
            format!("entry n{i}: {}", node.kind)
        } else {
            format!("n{i}: {}", node.kind)
        };
        out.push_str(&head);
        if args.is_empty() {
            out.push('\n');
        } else {
            // Constants render as an init block, one keyed value per line.
            out.push_str(" {\n");
            for arg in args {
                out.push_str(&format!("  {arg}\n"));
            }
            out.push_str("}\n");
        }
    }
    for edge in &blueprint.edges {
        let src = node_of(edge.source_node);
        let dst = node_of(edge.target_node);
        let src_idx = blueprint.nodes.iter().position(|n| n.id == src.id).unwrap();
        let dst_idx = blueprint.nodes.iter().position(|n| n.id == dst.id).unwrap();
        let src_pin = src.pins.iter().find(|p| p.id == edge.source_pin).unwrap();
        if src_pin.pin_type == PinType::ExecOutput {
            let pin = if src_pin.name == "Exec" {
                String::new()
            } else {
                format!(".{}", src_pin.name)
            };
            out.push_str(&format!("n{src_idx}{pin} -> n{dst_idx}\n"));
        } else {
            let dst_pin = dst.pins.iter().find(|p| p.id == edge.target_pin).unwrap();
            out.push_str(&format!("$n{dst_idx}.{} <- n{src_idx}.{}\n", dst_pin.name, src_pin.name));
        }
    }
    out
}
