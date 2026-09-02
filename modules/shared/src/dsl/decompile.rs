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
        let lit = |pin: &Pin| -> Option<String> {
            node.data
                .get(&pin.name)
                .and_then(|v| if v.is_null() { None } else { Some(v.to_string()) })
        };
        let args: Vec<String> = node
            .pins
            .iter()
            .filter(|p| p.pin_type == PinType::DataInput)
            .filter_map(|p| lit(p).map(|v| format!("{} = {v}", p.name)))
            .collect();
        let head = if node.id == entry.id {
            format!("entry n{i}: {}", node.kind)
        } else {
            format!("n{i}: {}", node.kind)
        };
        if args.is_empty() {
            out.push_str(&head);
        } else {
            out.push_str(&format!("{head}({})", args.join(", ")));
        }
        out.push('\n');
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
            out.push_str(&format!(
                "$n{dst_idx}.{} <- n{src_idx}.{}\n",
                dst_pin.name, src_pin.name
            ));
        }
    }
    out
}