//! Approval broker shared between running executions and the gRPC layer.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

/// A user decision on an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// The scope an approval decision applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only the current call.
    Once,
    /// The remainder of the current execution run.
    Run,
    /// Persisted in the workspace database.
    Workspace,
    /// Persisted in the global database.
    Global,
}

/// Parses a decision string from the `RespondApproval` RPC.
///
/// Accepts `AllowOnce`, `AllowRun`, `AllowWorkspace`, `AllowGlobal` and the
/// corresponding `Deny*` variants.
pub fn parse_decision(raw: &str) -> Option<(Decision, Scope)> {
    let (decision, rest) = if let Some(rest) = raw.strip_prefix("Allow") {
        (Decision::Allow, rest)
    } else {
        let rest = raw.strip_prefix("Deny")?;
        (Decision::Deny, rest)
    };
    let scope = match rest {
        "Once" => Scope::Once,
        "Run" => Scope::Run,
        "Workspace" => Scope::Workspace,
        "Global" => Scope::Global,
        _ => return None,
    };
    Some((decision, scope))
}

/// Metadata returned when a pending request is answered.
#[derive(Debug, Clone)]
pub struct RespondedRequest {
    /// Hash of the normalized command that triggered the request.
    pub command_hash: u64,
    /// The raw command line.
    pub command: String,
}

/// Tracks pending approval requests and run-scoped grants for one execution.
pub struct ApprovalBroker {
    pending: Mutex<HashMap<String, PendingRequest>>,
    run_grants: Mutex<HashMap<u64, bool>>,
}

struct PendingRequest {
    command_hash: u64,
    command: String,
    tx: oneshot::Sender<Decision>,
}

impl Default for ApprovalBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalBroker {
    /// Creates an empty broker.
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            run_grants: Mutex::new(HashMap::new()),
        }
    }

    /// Opens a new approval request, returning its id and the response slot.
    pub fn open_request(
        &self,
        command_hash: u64,
        command: impl Into<String>,
    ) -> (String, oneshot::Receiver<Decision>) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            request_id.clone(),
            PendingRequest {
                command_hash,
                command: command.into(),
                tx,
            },
        );
        (request_id, rx)
    }

    /// Delivers a decision to a pending request.
    ///
    /// Returns the request metadata so callers can persist scoped grants, or
    /// `None` when the request does not exist or was already answered.
    pub fn respond(&self, request_id: &str, decision: Decision) -> Option<RespondedRequest> {
        let pending = self.pending.lock().unwrap().remove(request_id)?;
        let _ = pending.tx.send(decision);
        Some(RespondedRequest {
            command_hash: pending.command_hash,
            command: pending.command,
        })
    }

    /// Records a run-scoped grant (`true` = allow) keyed by command hash.
    pub fn record_run_grant(&self, command_hash: u64, allow: bool) {
        self.run_grants.lock().unwrap().insert(command_hash, allow);
    }

    /// Looks up a run-scoped grant by command hash.
    pub fn run_grant(&self, command_hash: u64) -> Option<bool> {
        self.run_grants.lock().unwrap().get(&command_hash).copied()
    }

    /// Denies every pending request; used when an execution is torn down.
    pub fn deny_all(&self) {
        let mut pending = self.pending.lock().unwrap();
        for (_, request) in pending.drain() {
            let _ = request.tx.send(Decision::Deny);
        }
    }

    /// Returns the ids of unanswered requests (used by tests).
    pub fn pending_ids(&self) -> Vec<String> {
        self.pending.lock().unwrap().keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_decision_strings() {
        assert_eq!(parse_decision("AllowWorkspace"), Some((Decision::Allow, Scope::Workspace)));
        assert_eq!(parse_decision("DenyOnce"), Some((Decision::Deny, Scope::Once)));
        assert_eq!(parse_decision("allow"), None);
        assert_eq!(parse_decision("AllowSometimes"), None);
    }

    #[tokio::test]
    async fn respond_delivers_decision_once() {
        let broker = ApprovalBroker::new();
        let (id, rx) = broker.open_request(7, "git status");
        assert_eq!(id.len(), 36);
        let answered = broker.respond(&id, Decision::Allow).unwrap();
        assert_eq!(answered.command_hash, 7);
        assert_eq!(answered.command, "git status");
        assert_eq!(rx.await.unwrap(), Decision::Allow);
        // A second respond for the same id fails.
        assert!(broker.respond(&id, Decision::Deny).is_none());
    }

    #[test]
    fn run_grants_roundtrip() {
        let broker = ApprovalBroker::new();
        broker.record_run_grant(42, true);
        assert_eq!(broker.run_grant(42), Some(true));
        assert_eq!(broker.run_grant(43), None);
    }
}
