//! Filesystem abstraction over URIs.
//!
//! The [`FileSystem`] trait decouples consumers from the concrete storage
//! backend. The local [`NativeFileSystem`] implementation maps `file` URIs to
//! native paths; remote backends can be added later without changing callers.

use std::path::PathBuf;

use crate::error::SharedResult;
use crate::uri::Uri;

/// Metadata describing a filesystem entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Size in bytes (0 for directories).
    pub len: u64,
}

/// A filesystem that operates on [`Uri`] paths.
pub trait FileSystem: Send + Sync {
    /// Reads the full contents of the entry at `uri`.
    fn read(&self, uri: &Uri) -> SharedResult<Vec<u8>>;

    /// Writes `data` to the entry at `uri`, creating it if needed.
    fn write(&self, uri: &Uri, data: &[u8]) -> SharedResult<()>;

    /// Returns metadata for the entry at `uri`.
    fn metadata(&self, uri: &Uri) -> SharedResult<Metadata>;

    /// Lists the entries directly under the directory at `uri`.
    fn list(&self, uri: &Uri) -> SharedResult<Vec<Uri>>;

    /// Returns whether an entry exists at `uri`.
    fn exists(&self, uri: &Uri) -> bool;

    /// Creates the directory at `uri` (and any missing parents).
    fn create_dir_all(&self, uri: &Uri) -> SharedResult<()>;

    /// Removes the entry at `uri`.
    fn remove(&self, uri: &Uri) -> SharedResult<()>;

    /// Renames the entry at `from` to `to`.
    fn rename(&self, from: &Uri, to: &Uri) -> SharedResult<()>;
}

/// A [`FileSystem`] backed by the local operating system.
#[derive(Debug, Default, Clone)]
pub struct NativeFileSystem;

impl NativeFileSystem {
    /// Creates a new native filesystem.
    pub fn new() -> Self {
        Self
    }
}

impl FileSystem for NativeFileSystem {
    fn read(&self, uri: &Uri) -> SharedResult<Vec<u8>> {
        Ok(std::fs::read(uri.to_path()?)?)
    }

    fn write(&self, uri: &Uri, data: &[u8]) -> SharedResult<()> {
        let path = uri.to_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, data)?;
        Ok(())
    }

    fn metadata(&self, uri: &Uri) -> SharedResult<Metadata> {
        let meta = std::fs::metadata(uri.to_path()?)?;
        Ok(Metadata {
            is_dir: meta.is_dir(),
            len: meta.len(),
        })
    }

    fn list(&self, uri: &Uri) -> SharedResult<Vec<Uri>> {
        let path = uri.to_path()?;
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            out.push(Uri::from_path(&entry.path()));
        }
        Ok(out)
    }

    fn exists(&self, uri: &Uri) -> bool {
        uri.to_path().map(|p| p.exists()).unwrap_or(false)
    }

    fn create_dir_all(&self, uri: &Uri) -> SharedResult<()> {
        std::fs::create_dir_all(uri.to_path()?)?;
        Ok(())
    }

    fn remove(&self, uri: &Uri) -> SharedResult<()> {
        let path: PathBuf = uri.to_path()?;
        if path.is_dir() {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn rename(&self, from: &Uri, to: &Uri) -> SharedResult<()> {
        std::fs::rename(from.to_path()?, to.to_path()?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_file() {
        let fs = NativeFileSystem::new();
        let dir = std::env::temp_dir().join(format!("metteur-fs-{}", uuid::Uuid::new_v4()));
        let uri = Uri::from_path(&dir.join("a/b.txt"));
        fs.write(&uri, b"hello").unwrap();
        assert!(fs.exists(&uri));
        assert_eq!(fs.read(&uri).unwrap(), b"hello");
        let meta = fs.metadata(&uri).unwrap();
        assert!(!meta.is_dir);
        assert_eq!(meta.len, 5);
        fs.remove(&Uri::from_path(&dir)).unwrap();
    }
}
