//! Write-ahead transaction log for execution rollback.

use std::path::PathBuf;

use metteur_shared::Value;

/// A single recorded mutation during execution.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum TransactionEntry {
    /// A file was written (or created).
    FileWrite {
        /// The file path.
        path: PathBuf,
        /// The previous content, if the file already existed.
        old_content: Option<Vec<u8>>,
        /// The new content.
        new_content: Vec<u8>,
    },
    /// A file was deleted.
    FileDelete {
        /// The file path.
        path: PathBuf,
        /// The content before deletion.
        content: Vec<u8>,
    },
    /// A tool was invoked.
    ToolCall {
        /// The tool name.
        name: String,
        /// The tool arguments.
        args: Vec<Value>,
    },
}

/// A write-ahead log of mutations performed during an execution.
///
/// Cloning shares the same underlying entries, so nested runs (SubAgent /
/// abstract blueprints) record into the parent log and a rollback of the
/// outer run undoes their file mutations as well.
#[derive(Debug, Default, Clone)]
pub struct TransactionLog {
    entries: std::sync::Arc<std::sync::Mutex<Vec<TransactionEntry>>>,
}

impl TransactionLog {
    /// Creates an empty transaction log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a file write.
    pub fn record_file_write(
        &self,
        path: PathBuf,
        old_content: Option<Vec<u8>>,
        new_content: Vec<u8>,
    ) {
        self.lock().push(TransactionEntry::FileWrite {
            path,
            old_content,
            new_content,
        });
    }

    /// Records a file delete.
    pub fn record_file_delete(&self, path: PathBuf, content: Vec<u8>) {
        self.lock().push(TransactionEntry::FileDelete {
            path,
            content,
        });
    }

    /// Records a tool call.
    pub fn record_tool_call(&self, name: String, args: Vec<Value>) {
        self.lock().push(TransactionEntry::ToolCall {
            name,
            args,
        });
    }

    /// Returns a snapshot of the recorded entries.
    pub fn entries(&self) -> Vec<TransactionEntry> {
        self.lock().clone()
    }

    /// Creates a log from a previously recorded entry list.
    pub fn from_entries(entries: Vec<TransactionEntry>) -> Self {
        Self {
            entries: std::sync::Arc::new(std::sync::Mutex::new(entries)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<TransactionEntry>> {
        self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Returns the current log length, usable as a rollback mark.
    ///
    /// A later [`Self::rollback_after`] with this value undoes exactly the
    /// mutations recorded after the mark was taken.
    pub fn mark(&self) -> usize {
        self.lock().len()
    }

    /// Reverses all recorded mutations, restoring the pre-execution state.
    ///
    /// File writes restore the previous content (or delete the file if it did
    /// not exist); file deletes restore the deleted file.
    pub fn rollback(&self) -> std::io::Result<()> {
        self.rollback_after(0).map(|_| ())
    }

    /// Reverses the mutations recorded after `mark`, returning their count.
    ///
    /// Entries are replayed in reverse order, so multiple writes to the same
    /// file converge on the earliest before-image. This also holds for the
    /// duplicated entries a re-executed node records after a crash/resume.
    /// Shell side effects (e.g. `ExecuteCommand`) are not captured by the WAL
    /// and therefore cannot be undone.
    pub fn rollback_after(&self, mark: usize) -> std::io::Result<usize> {
        let entries = self.lock();
        let mut undone = 0;
        for entry in entries.iter().skip(mark).rev() {
            match entry {
                TransactionEntry::FileWrite {
                    path,
                    old_content,
                    ..
                } => {
                    match old_content {
                        Some(content) => std::fs::write(path, content)?,
                        None => {
                            if path.exists() {
                                std::fs::remove_file(path)?;
                            }
                        }
                    }
                    undone += 1;
                }
                TransactionEntry::FileDelete {
                    path,
                    content,
                } => {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(path, content)?;
                    undone += 1;
                }
                TransactionEntry::ToolCall {
                    ..
                } => {}
            }
        }
        Ok(undone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_restores_overwritten_file() {
        let dir = std::env::temp_dir().join(format!("metteur-txn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "v1").unwrap();

        let log = TransactionLog::new();
        log.record_file_write(path.clone(), Some(b"v1".to_vec()), b"v2".to_vec());
        std::fs::write(&path, "v2").unwrap();

        log.rollback().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");
    }

    #[test]
    fn rollback_removes_created_file() {
        let dir = std::env::temp_dir().join(format!("metteur-txn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("new.txt");

        let log = TransactionLog::new();
        log.record_file_write(path.clone(), None, b"data".to_vec());
        std::fs::write(&path, "data").unwrap();

        log.rollback().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn rollback_after_undoes_only_entries_past_the_mark() {
        let dir = std::env::temp_dir().join(format!("metteur-txn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let kept = dir.join("kept.txt");
        let reverted = dir.join("reverted.txt");
        std::fs::write(&kept, "v1").unwrap();

        let log = TransactionLog::new();
        log.record_file_write(kept.clone(), Some(b"v1".to_vec()), b"v2".to_vec());
        std::fs::write(&kept, "v2").unwrap();

        let mark = log.mark();
        log.record_file_write(reverted.clone(), None, b"tmp".to_vec());
        std::fs::write(&reverted, "tmp").unwrap();

        let undone = log.rollback_after(mark).unwrap();
        assert_eq!(undone, 1);
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "v2");
        assert!(!reverted.exists());
    }

    #[test]
    fn rollback_after_zero_matches_full_rollback() {
        let dir = std::env::temp_dir().join(format!("metteur-txn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "v1").unwrap();

        let log = TransactionLog::new();
        log.record_file_write(path.clone(), Some(b"v1".to_vec()), b"v2".to_vec());
        std::fs::write(&path, "v2").unwrap();

        log.rollback_after(0).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");
    }

    #[test]
    fn repeated_writes_converge_on_the_earliest_before_image() {
        let dir = std::env::temp_dir().join(format!("metteur-txn-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        std::fs::write(&path, "v1").unwrap();

        let log = TransactionLog::new();
        // Simulates a node executing twice (e.g. after a crash/resume) without
        // an intermediate rollback: both writes are recorded.
        log.record_file_write(path.clone(), Some(b"v1".to_vec()), b"v2".to_vec());
        log.record_file_write(path.clone(), Some(b"v2".to_vec()), b"v3".to_vec());
        std::fs::write(&path, "v3").unwrap();

        log.rollback().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");
    }
}
