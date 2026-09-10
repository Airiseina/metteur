//! Agent execution tree.
//!
//! The tree records the task hierarchy of a run (run → blueprint nodes →
//! function bodies / subagents) for the execution-tree view. It is built by
//! the interpreter alongside scheduling and persisted with checkpoints.

use std::collections::HashMap;

/// The kind of one tree node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TreeNodeKind {
    /// The run itself (tree root).
    Run,
    /// One blueprint node execution, carrying its node kind.
    BlueprintNode(String),
    /// A spawned subagent.
    SubAgent,
    /// An entered blueprint function, carrying its name.
    Function(String),
}

/// The lifecycle status of one tree node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TreeNodeStatus {
    /// Still executing.
    Running,
    /// Finished successfully.
    Done,
    /// Finished with an error.
    Failed(String),
}

/// One node of the execution tree.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecTreeNode {
    /// Stable id (`t1`, `t2`, …) unique within the run.
    pub id: String,
    /// What this node represents.
    pub kind: TreeNodeKind,
    /// Human-readable label.
    pub label: String,
    /// Parent node id, if any.
    pub parent: Option<String>,
    /// Child node ids in creation order.
    pub children: Vec<String>,
    /// Current lifecycle status.
    pub status: TreeNodeStatus,
    /// Accumulated LLM tokens (own + descendants).
    pub tokens: u64,
    /// Start time in milliseconds since the Unix epoch.
    pub started_at_ms: u64,
    /// Finish time, if the node completed.
    pub finished_at_ms: Option<u64>,
}

/// The task hierarchy of one execution run.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ExecTree {
    /// All nodes by id.
    pub nodes: HashMap<String, ExecTreeNode>,
    /// Root node ids in creation order.
    pub roots: Vec<String>,
    next_id: u64,
}

impl ExecTree {
    /// Creates an empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node under `parent` (`None` = new root), returning its id.
    pub fn spawn(
        &mut self,
        parent: Option<&str>,
        kind: TreeNodeKind,
        label: impl Into<String>,
        now_ms: u64,
    ) -> String {
        self.next_id += 1;
        let id = format!("t{}", self.next_id);
        let parent_exists = parent.is_some_and(|p| self.nodes.contains_key(p));
        let node = ExecTreeNode {
            id: id.clone(),
            kind,
            label: label.into(),
            // Drop a dangling parent so the node is not simultaneously an
            // orphan root and a child of a missing node.
            parent: if parent_exists {
                parent.map(str::to_string)
            } else {
                None
            },
            children: Vec::new(),
            status: TreeNodeStatus::Running,
            tokens: 0,
            started_at_ms: now_ms,
            finished_at_ms: None,
        };
        match parent.and_then(|p| self.nodes.get_mut(p)) {
            Some(existing) => existing.children.push(id.clone()),
            None => self.roots.push(id.clone()),
        }
        self.nodes.insert(id.clone(), node);
        id
    }

    /// Marks a node finished with `status`.
    pub fn finish(&mut self, id: &str, status: TreeNodeStatus, now_ms: u64) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.status = status;
            node.finished_at_ms = Some(now_ms);
        }
    }

    /// Adds `tokens` to a node and all of its ancestors.
    pub fn add_tokens(&mut self, id: &str, tokens: u64) {
        let mut current = Some(id.to_string());
        while let Some(next) = current {
            match self.nodes.get_mut(&next) {
                Some(node) => {
                    node.tokens += tokens;
                    current = node.parent.clone();
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_finish_and_token_rollup() {
        let mut tree = ExecTree::new();
        let run = tree.spawn(None, TreeNodeKind::Run, "run", 1);
        let child = tree.spawn(Some(&run), TreeNodeKind::SubAgent, "task", 2);
        tree.add_tokens(&child, 100);
        tree.finish(&child, TreeNodeStatus::Done, 3);
        tree.finish(&run, TreeNodeStatus::Done, 4);
        assert_eq!(tree.nodes[&child].tokens, 100);
        assert_eq!(tree.nodes[&run].tokens, 100);
        assert_eq!(tree.nodes[&run].children, vec![child.clone()]);
        assert!(matches!(tree.nodes[&child].status, TreeNodeStatus::Done));
    }
}

/// An operation an executor queues for the interpreter's tree.
///
/// Executors cannot touch the interpreter's tree directly; they push ops onto
/// the execution context and the interpreter drains them when the node
/// finishes, attributing children and tokens to the finished node.
#[derive(Debug, Clone)]
pub enum TreeOp {
    /// Add a child under the finished node and make it current.
    SpawnChild {
        /// What the child represents.
        kind: TreeNodeKind,
        /// Human-readable label.
        label: String,
    },
    /// Finish the current node with `status`, returning to its parent.
    FinishCurrent {
        /// Final status.
        status: TreeNodeStatus,
    },
    /// Add tokens to the current node (rolls up to ancestors).
    AddTokens(u64),
}
