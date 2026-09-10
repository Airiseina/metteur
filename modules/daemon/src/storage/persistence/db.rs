//! RocksDB persistence wrapper.

use std::path::Path;
use std::sync::Arc;

use rocksdb::{ColumnFamilyDescriptor, DB};

use crate::error::{DaemonError, DaemonResult};

/// Column family names used by the daemon.
pub mod cf {
    /// Stores blueprints keyed by blueprint id.
    pub const BLUEPRINTS: &str = "blueprints";
    /// Stores version snapshot metadata.
    pub const SNAPSHOTS: &str = "snapshots";
    /// Stores file content blobs keyed by content hash.
    pub const FILE_BLOBS: &str = "file_blobs";
    /// Stores audit log entries.
    pub const AUDIT_LOG: &str = "audit_log";
    /// Stores execution checkpoints keyed by run id.
    pub const EXECUTION_STATE: &str = "execution_state";
    /// Stores persistent sandbox grants keyed by command hash.
    pub const GRANTS: &str = "grants";
    /// Stores blueprint functions keyed by function name.
    pub const FUNCTIONS: &str = "functions";
    /// Stores the workspace's active chat session.
    pub const CHAT_SESSIONS: &str = "chat_sessions";
    /// Stores chat threads keyed by session id.
    pub const CHAT_THREADS: &str = "chat_threads";
}

/// A thin wrapper around a RocksDB instance with typed column families.
#[derive(Clone)]
pub struct Db {
    inner: Arc<DB>,
}

impl Db {
    /// Opens (or creates) a database at `path` with the standard column
    /// families.
    pub fn open(path: &Path) -> DaemonResult<Self> {
        std::fs::create_dir_all(path)?;
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let cfs = [
            ColumnFamilyDescriptor::new(cf::BLUEPRINTS, rocksdb::Options::default()),
            ColumnFamilyDescriptor::new(cf::SNAPSHOTS, rocksdb::Options::default()),
            ColumnFamilyDescriptor::new(cf::FILE_BLOBS, rocksdb::Options::default()),
            ColumnFamilyDescriptor::new(cf::AUDIT_LOG, rocksdb::Options::default()),
            ColumnFamilyDescriptor::new(cf::EXECUTION_STATE, rocksdb::Options::default()),
            ColumnFamilyDescriptor::new(cf::GRANTS, rocksdb::Options::default()),
            ColumnFamilyDescriptor::new(cf::FUNCTIONS, rocksdb::Options::default()),
            ColumnFamilyDescriptor::new(cf::CHAT_SESSIONS, rocksdb::Options::default()),
            ColumnFamilyDescriptor::new(cf::CHAT_THREADS, rocksdb::Options::default()),
        ];

        // RocksDB on Windows rejects the `\\?\` extended-length path prefix
        // that `canonicalize` may produce, so normalize it away.
        let db_path = normalize_path(path);
        let db = DB::open_cf_descriptors(&opts, &db_path, cfs)
            .map_err(|e| DaemonError::Internal(format!("failed to open db: {e}")))?;
        Ok(Self {
            inner: Arc::new(db),
        })
    }

    /// Puts a value into a column family.
    pub fn put(&self, cf: &str, key: &[u8], value: &[u8]) -> DaemonResult<()> {
        let handle = self
            .inner
            .cf_handle(cf)
            .ok_or_else(|| DaemonError::Internal(format!("unknown column family {cf}")))?;
        self.inner
            .put_cf(handle, key, value)
            .map_err(|e| DaemonError::Internal(format!("db put failed: {e}")))?;
        Ok(())
    }

    /// Gets a value from a column family.
    pub fn get(&self, cf: &str, key: &[u8]) -> DaemonResult<Option<Vec<u8>>> {
        let handle = self
            .inner
            .cf_handle(cf)
            .ok_or_else(|| DaemonError::Internal(format!("unknown column family {cf}")))?;
        self.inner
            .get_cf(handle, key)
            .map_err(|e| DaemonError::Internal(format!("db get failed: {e}")))
    }

    /// Deletes a key from a column family.
    ///
    /// Deleting is idempotent: removing a missing key succeeds silently.
    pub fn delete(&self, cf: &str, key: &[u8]) -> DaemonResult<()> {
        let handle = self
            .inner
            .cf_handle(cf)
            .ok_or_else(|| DaemonError::Internal(format!("unknown column family {cf}")))?;
        self.inner
            .delete_cf(handle, key)
            .map_err(|e| DaemonError::Internal(format!("db delete failed: {e}")))?;
        Ok(())
    }

    /// Iterates over all key-value pairs in a column family.
    pub fn scan(&self, cf: &str) -> DaemonResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let handle = self
            .inner
            .cf_handle(cf)
            .ok_or_else(|| DaemonError::Internal(format!("unknown column family {cf}")))?;
        let iter = self.inner.iterator_cf(handle, rocksdb::IteratorMode::Start);
        let mut out = Vec::new();
        for item in iter {
            let (k, v) = item.map_err(|e| DaemonError::Internal(format!("db scan failed: {e}")))?;
            out.push((k.to_vec(), v.to_vec()));
        }
        Ok(out)
    }
}

/// Converts a path to a string usable by RocksDB.
///
/// On Windows, strips the `\\?\` extended-length prefix that `canonicalize`
/// may add, which RocksDB does not accept.
fn normalize_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    #[cfg(windows)]
    {
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
    }
    #[cfg(not(windows))]
    {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_roundtrip() {
        let dir = std::env::temp_dir().join(format!("metteur-db-{}", uuid::Uuid::new_v4()));
        let db = Db::open(&dir).unwrap();
        db.put(cf::BLUEPRINTS, b"key1", b"value1").unwrap();
        assert_eq!(db.get(cf::BLUEPRINTS, b"key1").unwrap().unwrap(), b"value1");
    }
}
