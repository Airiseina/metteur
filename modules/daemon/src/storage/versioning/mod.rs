//! Version management based on content-addressed file snapshots.
//!
//! Each snapshot stores a manifest mapping relative file paths to content
//! hashes. File contents are stored once in a deduplicated blob store keyed
//! by hash (similar to git's object model). Creating a snapshot skips files
//! whose metadata (mtime + size) is unchanged since the previous snapshot,
//! avoiding re-reading and re-hashing untouched files.
//!
//! The optional [`watcher`] module turns file system changes into automatic
//! snapshots.

pub mod watcher;

use std::collections::{HashMap, HashSet};
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use twox_hash::XxHash64;
use uuid::Uuid;

use crate::error::{DaemonError, DaemonResult};
use crate::storage::persistence::{Db, cf};

pub use watcher::WorkspaceWatcher;

/// Metadata describing a version snapshot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    /// Unique snapshot id.
    pub id: Uuid,
    /// Human-readable description.
    pub description: String,
    /// Optional alias for referencing this snapshot on rollback.
    pub alias: Option<String>,
    /// Creation time in microseconds since the Unix epoch.
    ///
    /// Microsecond precision keeps the snapshot list a strict total order even
    /// when several snapshots are created back-to-back; clients divide by
    /// 1000 for the millisecond display value.
    pub created_at: u64,
    /// File manifest: relative path -> content hash.
    pub files: HashMap<String, String>,
    /// File metadata for change detection: relative path -> (mtime nanos, size).
    pub file_meta: HashMap<String, (u128, u64)>,
}

/// The change status of a tracked file between consecutive snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeStatus {
    /// The file appeared in this snapshot.
    Added,
    /// The file's content changed in this snapshot.
    Modified,
    /// The file was removed in this snapshot.
    Deleted,
    /// The file is unchanged in this snapshot.
    Unchanged,
}

/// One entry of a tracked file's history across snapshots.
#[derive(Debug, Clone)]
pub struct FileHistoryEntry {
    /// The snapshot this entry refers to.
    pub snapshot: Snapshot,
    /// The file's status in that snapshot.
    pub status: FileChangeStatus,
    /// The content hash, if the file exists in the snapshot.
    pub hash: Option<String>,
}

/// Manages version snapshots for a single workspace.
pub struct VersionManager {
    db: Db,
    root: PathBuf,
}

impl VersionManager {
    /// Creates a version manager backed by the workspace database.
    pub fn new(db: Db, root: PathBuf) -> Self {
        Self {
            db,
            root,
        }
    }

    /// Creates a snapshot of the current workspace file state.
    ///
    /// Equivalent to [`Self::create_snapshot_with`] without an alias.
    pub fn create_snapshot(&self, description: &str) -> DaemonResult<Snapshot> {
        self.create_snapshot_with(description, None)
    }

    /// Creates a snapshot with an optional alias used for rollback.
    ///
    /// Files whose metadata is unchanged since the previous snapshot reuse
    /// their stored content hash; changed and new files are read, hashed and
    /// stored in the deduplicated blob store. Deleted files are simply absent
    /// from the new manifest.
    pub fn create_snapshot_with(
        &self,
        description: &str,
        alias: Option<&str>,
    ) -> DaemonResult<Snapshot> {
        let (snapshot, _) = self.build_snapshot(description, alias)?;
        self.store_snapshot(&snapshot)?;
        Ok(snapshot)
    }

    /// Creates a snapshot only if the workspace actually changed.
    ///
    /// Returns `None` when the file manifest is identical to the previous
    /// snapshot, avoiding noise from file watchers and idle runs.
    pub fn create_snapshot_if_changed(&self, description: &str) -> DaemonResult<Option<Snapshot>> {
        let (snapshot, changed) = self.build_snapshot(description, None)?;
        if !changed {
            return Ok(None);
        }
        self.store_snapshot(&snapshot)?;
        Ok(Some(snapshot))
    }

    /// Computes a snapshot without persisting it.
    ///
    /// Returns `(snapshot, changed)` where `changed` is false when the file
    /// manifest equals that of the previous snapshot.
    fn build_snapshot(
        &self,
        description: &str,
        alias: Option<&str>,
    ) -> DaemonResult<(Snapshot, bool)> {
        let previous = self.latest_snapshot()?;
        let mut files = HashMap::new();
        let mut file_meta = HashMap::new();

        for file in self.collect_files(&self.root)? {
            let rel = file
                .strip_prefix(&self.root)
                .map_err(|_| DaemonError::Internal("path outside workspace".to_string()))?;
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let meta = std::fs::metadata(&file)?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let current_meta = (mtime, meta.len());

            // Reuse the previous hash when the file is unchanged.
            let hash = match &previous {
                Some(prev) if prev.file_meta.get(&rel_str) == Some(&current_meta) => {
                    match prev.files.get(&rel_str) {
                        Some(h) => h.clone(),
                        None => self.store_blob(&file)?,
                    }
                }
                _ => self.store_blob(&file)?,
            };

            files.insert(rel_str.clone(), hash);
            file_meta.insert(rel_str, current_meta);
        }

        let changed = previous.as_ref().map(|prev| prev.files != files).unwrap_or(true);
        let snapshot = Snapshot {
            id: Uuid::new_v4(),
            description: description.to_string(),
            alias: alias.map(str::to_string),
            created_at: now_micros(),
            files,
            file_meta,
        };
        Ok((snapshot, changed))
    }

    /// Serializes and stores a snapshot.
    fn store_snapshot(&self, snapshot: &Snapshot) -> DaemonResult<()> {
        let meta =
            serde_json::to_vec(snapshot).map_err(|e| DaemonError::Serialization(e.to_string()))?;
        self.db.put(cf::SNAPSHOTS, snapshot.id.as_bytes(), &meta)
    }

    /// Returns the change history of a tracked file across all snapshots.
    ///
    /// A file is considered tracked if it appears in at least one snapshot.
    pub fn file_history(&self, rel: &str) -> DaemonResult<Vec<FileHistoryEntry>> {
        let snapshots = self.list_snapshots()?;
        let tracked = snapshots.iter().any(|s| s.files.contains_key(rel));
        if !tracked {
            return Ok(Vec::new());
        }

        let mut out = Vec::new();
        let mut prev_hash: Option<String> = None;
        for snapshot in snapshots {
            let hash = snapshot.files.get(rel).cloned();
            if let Some(status) = file_change_status(prev_hash.as_deref(), hash.as_deref()) {
                out.push(FileHistoryEntry {
                    snapshot,
                    status,
                    hash: hash.clone(),
                });
            }
            if let Some(h) = hash {
                prev_hash = Some(h);
            }
        }
        Ok(out)
    }

    /// Reads a file, stores its content in the blob store if absent, and
    /// returns its content hash.
    fn store_blob(&self, file: &Path) -> DaemonResult<String> {
        let data = std::fs::read(file)?;
        let hash = hash_content(&data);
        if self.db.get(cf::FILE_BLOBS, hash.as_bytes())?.is_none() {
            self.db.put(cf::FILE_BLOBS, hash.as_bytes(), &data)?;
        }
        Ok(hash)
    }

    /// Lists all snapshots for the workspace, ordered by creation time.
    pub fn list_snapshots(&self) -> DaemonResult<Vec<Snapshot>> {
        let mut out = Vec::new();
        for (_, value) in self.db.scan(cf::SNAPSHOTS)? {
            let snapshot: Snapshot = serde_json::from_slice(&value)
                .map_err(|e| DaemonError::Serialization(e.to_string()))?;
            out.push(snapshot);
        }
        out.sort_by_key(|s| s.created_at);
        Ok(out)
    }

    /// Returns the most recent snapshot, if any.
    fn latest_snapshot(&self) -> DaemonResult<Option<Snapshot>> {
        Ok(self.list_snapshots()?.pop())
    }

    /// Restores the workspace files to the state of the given snapshot.
    ///
    /// Files in the snapshot's manifest are written from their blobs; files
    /// tracked in any snapshot but absent from this one are deleted.
    pub fn rollback(&self, snapshot_id: Uuid) -> DaemonResult<()> {
        let meta = self
            .db
            .get(cf::SNAPSHOTS, snapshot_id.as_bytes())?
            .ok_or_else(|| DaemonError::NotFound(format!("snapshot {snapshot_id}")))?;
        let snapshot: Snapshot =
            serde_json::from_slice(&meta).map_err(|e| DaemonError::Serialization(e.to_string()))?;

        self.rollback_snapshot(&snapshot)
    }

    /// Restores to the snapshot matched by the given alias.
    ///
    /// Returns an error if no snapshot has this alias (aliases are not
    /// required to be unique; the last match wins).
    pub fn rollback_by_alias(&self, alias: &str) -> DaemonResult<()> {
        let snapshot = self
            .find_snapshot_by_alias(alias)?
            .ok_or_else(|| DaemonError::NotFound(format!("snapshot with alias {alias}")))?;
        self.rollback_snapshot(&snapshot)
    }

    /// Finds the most recent snapshot matching the given alias.
    ///
    /// Returns `None` if no snapshot matches. Because aliases are not unique,
    /// the last (most recent) match wins.
    pub fn find_snapshot_by_alias(&self, alias: &str) -> DaemonResult<Option<Snapshot>> {
        let snapshots = self.list_snapshots()?;
        Ok(snapshots.into_iter().rfind(|s| s.alias.as_deref() == Some(alias)))
    }

    fn rollback_snapshot(&self, snapshot: &Snapshot) -> DaemonResult<()> {
        // Restore each file from its blob.
        for (rel, hash) in &snapshot.files {
            let blob = self
                .db
                .get(cf::FILE_BLOBS, hash.as_bytes())?
                .ok_or_else(|| DaemonError::NotFound(format!("blob {hash}")))?;
            let target = self.root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, blob)?;
        }

        // Delete files tracked in any snapshot but absent from this one.
        for rel in self.all_tracked_files()? {
            if !snapshot.files.contains_key(&rel) {
                let target = self.root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
                if target.exists() {
                    std::fs::remove_file(target)?;
                }
            }
        }
        Ok(())
    }

    /// Returns the set of file paths tracked across all snapshots.
    fn all_tracked_files(&self) -> DaemonResult<HashSet<String>> {
        let mut out = HashSet::new();
        for snapshot in self.list_snapshots()? {
            out.extend(snapshot.files.keys().cloned());
        }
        Ok(out)
    }

    /// Recursively collects the files under `dir`.
    fn collect_files(&self, dir: &Path) -> DaemonResult<Vec<PathBuf>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                // Skip the metadata directory to avoid self-referential data.
                if path.file_name().map(|n| n == ".metteur").unwrap_or(false) {
                    continue;
                }
                out.extend(self.collect_files(&path)?);
            } else {
                out.push(path);
            }
        }
        Ok(out)
    }
}

/// Derives a file's change status between consecutive snapshots.
///
/// `prev` and `cur` are the file's content hash in the previous and current
/// snapshot. Returns `None` when the file is absent from both, in which case
/// no history entry is produced for that snapshot.
fn file_change_status(prev: Option<&str>, cur: Option<&str>) -> Option<FileChangeStatus> {
    match (prev, cur) {
        (None, None) => None,
        (None, Some(_)) => Some(FileChangeStatus::Added),
        (Some(p), Some(c)) if p == c => Some(FileChangeStatus::Unchanged),
        (Some(_), Some(_)) => Some(FileChangeStatus::Modified),
        (Some(_), None) => Some(FileChangeStatus::Deleted),
    }
}

/// Computes a content hash for a byte buffer.
fn hash_content(data: &[u8]) -> String {
    let mut hasher = XxHash64::with_seed(0);
    hasher.write(data);
    format!("{:016x}", hasher.finish())
}

/// Returns the current time in microseconds since the Unix epoch.
fn now_micros() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("metteur-ver-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_and_lists_snapshots() {
        let root = temp_workspace();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = VersionManager::new(db, root.clone());

        let snap = manager.create_snapshot("initial").unwrap();
        assert_eq!(manager.list_snapshots().unwrap().len(), 1);
        assert_eq!(snap.description, "initial");
    }

    #[test]
    fn rollback_restores_files() {
        let root = temp_workspace();
        std::fs::write(root.join("a.txt"), "v1").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = VersionManager::new(db, root.clone());

        let snap = manager.create_snapshot("v1").unwrap();
        std::fs::write(root.join("a.txt"), "v2").unwrap();

        manager.rollback(snap.id).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "v1");
    }

    #[test]
    fn tracks_file_deletion() {
        let root = temp_workspace();
        std::fs::write(root.join("a.txt"), "content").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = VersionManager::new(db, root.clone());

        let snap1 = manager.create_snapshot("with file").unwrap();
        std::fs::remove_file(root.join("a.txt")).unwrap();
        let snap2 = manager.create_snapshot("file deleted").unwrap();

        // Roll back to the deletion snapshot: the file stays deleted.
        manager.rollback(snap2.id).unwrap();
        assert!(!root.join("a.txt").exists());

        // Roll back to the earlier snapshot: the file is restored.
        manager.rollback(snap1.id).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "content");
    }

    #[test]
    fn tracks_delete_then_recreate() {
        let root = temp_workspace();
        std::fs::write(root.join("a.txt"), "v1").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = VersionManager::new(db, root.clone());

        let snap1 = manager.create_snapshot("v1").unwrap();
        std::fs::remove_file(root.join("a.txt")).unwrap();
        let snap2 = manager.create_snapshot("deleted").unwrap();
        std::fs::write(root.join("a.txt"), "v2").unwrap();
        let snap3 = manager.create_snapshot("recreated").unwrap();

        // Roll back to the re-creation snapshot: the file has the new content.
        manager.rollback(snap3.id).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "v2");

        // Roll back to the deletion snapshot: the file is deleted.
        manager.rollback(snap2.id).unwrap();
        assert!(!root.join("a.txt").exists());

        // Roll back to the first snapshot: the original content is restored.
        manager.rollback(snap1.id).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "v1");
    }

    #[test]
    fn deduplicates_identical_content() {
        let root = temp_workspace();
        std::fs::write(root.join("a.txt"), "same").unwrap();
        std::fs::write(root.join("b.txt"), "same").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = VersionManager::new(db, root.clone());

        let snap = manager.create_snapshot("dedup").unwrap();
        // Two files with identical content share one blob.
        let hashes: HashSet<&String> = snap.files.values().collect();
        assert_eq!(hashes.len(), 1);
    }

    #[test]
    fn if_changed_skips_unchanged_workspace() {
        let root = temp_workspace();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = VersionManager::new(db, root.clone());

        let first = manager.create_snapshot_if_changed("s1").unwrap();
        assert!(first.is_some());
        // No file changed since the previous snapshot.
        let second = manager.create_snapshot_if_changed("s2").unwrap();
        assert!(second.is_none());
        assert_eq!(manager.list_snapshots().unwrap().len(), 1);
    }

    #[test]
    fn if_changed_snapshots_modifications() {
        let root = temp_workspace();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = VersionManager::new(db, root.clone());

        let first = manager.create_snapshot_if_changed("s1").unwrap();
        assert!(first.is_some());
        std::fs::write(root.join("a.txt"), "changed content").unwrap();
        let second = manager.create_snapshot_if_changed("s2").unwrap();
        assert!(second.is_some());
    }

    #[test]
    fn file_history_tracks_timeline() {
        let root = temp_workspace();
        std::fs::write(root.join("a.txt"), "v1").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = VersionManager::new(db, root.clone());

        let _s1 = manager.create_snapshot("added").unwrap();
        std::fs::write(root.join("a.txt"), "a longer second version").unwrap();
        let _s2 = manager.create_snapshot("modified").unwrap();
        std::fs::remove_file(root.join("a.txt")).unwrap();
        let _s3 = manager.create_snapshot("deleted").unwrap();

        let history = manager.file_history("a.txt").unwrap();
        let statuses: Vec<FileChangeStatus> = history.iter().map(|e| e.status).collect();
        assert_eq!(
            statuses,
            vec![FileChangeStatus::Added, FileChangeStatus::Modified, FileChangeStatus::Deleted,]
        );
    }

    #[test]
    fn file_history_untracked_file_is_empty() {
        let root = temp_workspace();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = VersionManager::new(db, root.clone());
        manager.create_snapshot("s1").unwrap();
        assert!(manager.file_history("b.txt").unwrap().is_empty());
    }

    #[test]
    fn file_change_status_matrix() {
        assert_eq!(file_change_status(None, None), None);
        assert_eq!(file_change_status(None, Some("h")), Some(FileChangeStatus::Added));
        assert_eq!(file_change_status(Some("h"), Some("h")), Some(FileChangeStatus::Unchanged));
        assert_eq!(file_change_status(Some("h"), Some("h2")), Some(FileChangeStatus::Modified));
        assert_eq!(file_change_status(Some("h"), None), Some(FileChangeStatus::Deleted));
    }

    #[test]
    fn alias_create_find_and_rollback() {
        let root = temp_workspace();
        std::fs::write(root.join("a.txt"), "v1").unwrap();
        let db = crate::storage::persistence::Db::open(&root.join(".metteur/db")).unwrap();
        let manager = VersionManager::new(db, root.clone());

        let snap = manager.create_snapshot_with("release", Some("good")).unwrap();
        assert_eq!(snap.alias.as_deref(), Some("good"));

        // A snapshot without an alias matches nothing by alias.
        manager.create_snapshot_with("interim", None).unwrap();
        let found = manager.find_snapshot_by_alias("good").unwrap().unwrap();
        assert_eq!(found.id, snap.id);
        assert!(manager.find_snapshot_by_alias("missing").unwrap().is_none());

        std::fs::write(root.join("a.txt"), "v2").unwrap();
        manager.rollback_by_alias("good").unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "v1");
    }
}
