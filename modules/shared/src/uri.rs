//! URI type for the Metteur filesystem abstraction.
//!
//! URIs follow RFC 3987 (IRI) and use the `file` scheme, for example
//! `file:///C:/Windows/`. The type is deliberately small and self-contained:
//! it parses a URI into scheme, authority and path components, and can
//! convert to and from native filesystem paths.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{SharedError, SharedResult};

/// A parsed URI in the Metteur filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Uri {
    scheme: String,
    authority: String,
    path: String,
}

impl Uri {
    /// Parses a URI string such as `file:///C:/Windows/`.
    pub fn parse(input: &str) -> SharedResult<Self> {
        let (scheme, rest) =
            input.split_once(':').ok_or_else(|| SharedError::InvalidUri(input.to_string()))?;
        if scheme.is_empty() {
            return Err(SharedError::InvalidUri(input.to_string()));
        }

        let (authority, path) = if let Some(rest) = rest.strip_prefix("//") {
            match rest.find('/') {
                Some(idx) => (rest[..idx].to_string(), rest[idx..].to_string()),
                None => (rest.to_string(), String::new()),
            }
        } else {
            (String::new(), rest.to_string())
        };

        Ok(Self {
            scheme: scheme.to_string(),
            authority,
            path,
        })
    }

    /// Returns the URI scheme, e.g. `file`.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Returns the authority component (empty for local `file` URIs).
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// Returns the path component.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Converts a native filesystem path into a `file` URI.
    pub fn from_path(path: &Path) -> Self {
        let path_str = path.to_string_lossy().replace('\\', "/");
        let path_str = if path_str.starts_with('/') {
            path_str
        } else {
            format!("/{path_str}")
        };
        Self {
            scheme: "file".to_string(),
            authority: String::new(),
            path: path_str,
        }
    }

    /// Converts this URI into a native filesystem path.
    ///
    /// Only `file` scheme URIs with an empty authority are supported.
    pub fn to_path(&self) -> SharedResult<PathBuf> {
        if self.scheme != "file" {
            return Err(SharedError::Unsupported(format!(
                "scheme '{}' is not supported for local paths",
                self.scheme
            )));
        }
        if !self.authority.is_empty() {
            return Err(SharedError::Unsupported("remote authority is not supported".to_string()));
        }
        #[cfg(windows)]
        {
            // Windows absolute paths have no leading slash; map `/` to the
            // native separator.
            let path = self.path.trim_start_matches('/');
            Ok(PathBuf::from(path.replace('/', "\\")))
        }
        #[cfg(not(windows))]
        {
            Ok(PathBuf::from(&self.path))
        }
    }
}

impl fmt::Display for Uri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.authority.is_empty() {
            write!(f, "{}:{}", self.scheme, self.path)
        } else {
            write!(f, "{}://{}{}", self.scheme, self.authority, self.path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_file_uri() {
        let uri = Uri::parse("file:///C:/Windows/").unwrap();
        assert_eq!(uri.scheme(), "file");
        assert_eq!(uri.authority(), "");
        assert_eq!(uri.path(), "/C:/Windows/");
    }

    #[test]
    fn parses_uri_with_authority() {
        let uri = Uri::parse("metteur://remote/path/to/file").unwrap();
        assert_eq!(uri.scheme(), "metteur");
        assert_eq!(uri.authority(), "remote");
        assert_eq!(uri.path(), "/path/to/file");
    }

    #[test]
    fn rejects_uri_without_scheme() {
        assert!(Uri::parse("no-scheme").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn round_trips_windows_path() {
        let path = Path::new("C:/Windows/System32");
        let uri = Uri::from_path(path);
        assert_eq!(uri.to_path().unwrap(), PathBuf::from("C:/Windows/System32"));
    }

    #[cfg(not(windows))]
    #[test]
    fn round_trips_unix_path() {
        let path = Path::new("/usr/local/bin");
        let uri = Uri::from_path(path);
        assert_eq!(uri.to_path().unwrap(), PathBuf::from("/usr/local/bin"));
    }
}
