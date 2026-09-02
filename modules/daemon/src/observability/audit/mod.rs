//! Audit logging for the daemon.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{DaemonError, DaemonResult};
use crate::storage::persistence::{Db, cf};

/// A single audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Creation time in milliseconds since the Unix epoch.
    pub timestamp: u64,
    /// The user id associated with the operation.
    pub user_id: String,
    /// The operation type (e.g. `workspace.open`, `execution.start`).
    pub operation: String,
    /// Operation-specific details as JSON.
    pub detail: serde_json::Value,
}

/// Writes audit entries to a database's `audit_log` column family.
#[derive(Clone)]
pub struct AuditWriter {
    db: Db,
}

impl AuditWriter {
    /// Creates an audit writer backed by the given database.
    pub fn new(db: Db) -> Self {
        Self {
            db,
        }
    }

    /// Records an audit entry.
    pub fn record(
        &self,
        user_id: &str,
        operation: &str,
        detail: serde_json::Value,
    ) -> DaemonResult<()> {
        let entry = AuditEntry {
            timestamp: now_millis(),
            user_id: user_id.to_string(),
            operation: operation.to_string(),
            detail,
        };
        let value =
            serde_json::to_vec(&entry).map_err(|e| DaemonError::Serialization(e.to_string()))?;
        // Key by `timestamp:uuid` to keep entries roughly ordered and unique.
        let key = format!("{}:{}", entry.timestamp, uuid::Uuid::new_v4());
        self.db.put(cf::AUDIT_LOG, key.as_bytes(), &value)
    }

    /// Returns all audit entries, ordered by timestamp.
    pub fn list(&self) -> DaemonResult<Vec<AuditEntry>> {
        let mut out = Vec::new();
        for (_, value) in self.db.scan(cf::AUDIT_LOG)? {
            let entry: AuditEntry = serde_json::from_slice(&value)
                .map_err(|e| DaemonError::Serialization(e.to_string()))?;
            out.push(entry);
        }
        out.sort_by_key(|e| e.timestamp);
        Ok(out)
    }
}

/// Returns the current time in milliseconds since the Unix epoch.
fn now_millis() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_lists_entries() {
        let dir = std::env::temp_dir().join(format!("metteur-audit-{}", uuid::Uuid::new_v4()));
        let db = crate::storage::persistence::Db::open(&dir).unwrap();
        let writer = AuditWriter::new(db);
        writer.record("user1", "execution.start", serde_json::json!({"blueprint": "b1"})).unwrap();
        let entries = writer.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].operation, "execution.start");
        assert_eq!(entries[0].user_id, "user1");
    }
}
