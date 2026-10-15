//! Immutable, caller supplied repository snapshots and their content identity.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// A stable digest of every declared path, mode, and content in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotIdentity(String);

impl SnapshotIdentity {
    /// Return the lowercase hexadecimal digest.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SnapshotIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One file or symlink declared in a repository snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    path: String,
    mode: u32,
    content: Vec<u8>,
}

impl SnapshotEntry {
    /// Construct a repository-relative snapshot entry.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::InvalidPath`] for an empty, absolute, or
    /// traversal path.
    pub fn new(
        path: impl Into<String>,
        mode: u32,
        content: impl Into<Vec<u8>>,
    ) -> Result<Self, SnapshotError> {
        let path = path.into();
        validate_path(&path)?;
        Ok(Self {
            path,
            mode,
            content: content.into(),
        })
    }

    /// Repository-relative path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Git-compatible file mode, such as `0o100644`, `0o100755`, or `0o120000`.
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    /// Full unmodified entry bytes.
    pub fn content(&self) -> &[u8] {
        &self.content
    }
}

/// Why a caller supplied snapshot could not be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// The path is not a safe repository-relative path.
    InvalidPath(String),
    /// The same path was declared more than once.
    DuplicatePath(String),
    /// Reading or inspecting an execution-root path failed.
    Io {
        /// Path that could not be read.
        path: String,
        /// Operating-system error description.
        message: String,
    },
    /// The execution root contains a filesystem entry that is not a regular
    /// file or symlink.
    UnsupportedEntry(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(f, "invalid repository-relative path: {path:?}"),
            Self::DuplicatePath(path) => write!(f, "duplicate snapshot path: {path:?}"),
            Self::Io { path, message } => write!(f, "could not read {path:?}: {message}"),
            Self::UnsupportedEntry(path) => {
                write!(f, "unsupported filesystem entry in snapshot: {path:?}")
            }
        }
    }
}

impl std::error::Error for SnapshotError {}

/// An immutable set of repository input files supplied by an embedded caller.
///
/// Paths are sorted internally, so identity does not depend on input order.
/// The identity includes each path, mode, and complete content bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    entries: BTreeMap<String, SnapshotEntry>,
    identity: SnapshotIdentity,
}

impl Snapshot {
    /// Build a snapshot from its complete declared input entries.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or repeated path.
    pub fn new(entries: impl IntoIterator<Item = SnapshotEntry>) -> Result<Self, SnapshotError> {
        let mut by_path = BTreeMap::new();
        for entry in entries {
            let path = entry.path.clone();
            if by_path.insert(path.clone(), entry).is_some() {
                return Err(SnapshotError::DuplicatePath(path));
            }
        }
        let identity = SnapshotIdentity(identity_of(&by_path));
        Ok(Self {
            entries: by_path,
            identity,
        })
    }

    /// Read a materialized candidate tree and compute its full snapshot.
    ///
    /// The root-level `.git` directory is excluded as repository metadata.
    /// Other excluded paths must be listed explicitly; a path excludes itself
    /// and all descendants. Directories are traversed without following
    /// symlinks.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] if paths cannot be read or represented as
    /// UTF-8 repository-relative names, or if the tree contains unsupported
    /// filesystem entries.
    pub fn from_directory(root: &Path, ignored_paths: &[String]) -> Result<Self, SnapshotError> {
        for path in ignored_paths {
            validate_path(path)?;
        }
        let mut entries = Vec::new();
        let mut pending = vec![(root.to_path_buf(), String::new())];
        while let Some((directory, parent)) = pending.pop() {
            let read_dir = std::fs::read_dir(&directory).map_err(|error| SnapshotError::Io {
                path: directory.display().to_string(),
                message: error.to_string(),
            })?;
            let mut children = read_dir
                .map(|entry| {
                    entry.map_err(|error| SnapshotError::Io {
                        path: directory.display().to_string(),
                        message: error.to_string(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            children.sort_by_key(|entry| entry.file_name());
            for child in children {
                let name = child.file_name().into_string().map_err(|name| {
                    SnapshotError::InvalidPath(name.to_string_lossy().into_owned())
                })?;
                let relative = if parent.is_empty() {
                    name
                } else {
                    format!("{parent}/{name}")
                };
                if relative == ".git"
                    || relative.starts_with(".git/")
                    || is_ignored(&relative, ignored_paths)
                {
                    continue;
                }
                let path = child.path();
                let metadata =
                    std::fs::symlink_metadata(&path).map_err(|error| SnapshotError::Io {
                        path: path.display().to_string(),
                        message: error.to_string(),
                    })?;
                if metadata.file_type().is_symlink() {
                    let target = std::fs::read_link(&path).map_err(|error| SnapshotError::Io {
                        path: path.display().to_string(),
                        message: error.to_string(),
                    })?;
                    #[cfg(unix)]
                    let content = {
                        use std::os::unix::ffi::OsStrExt;
                        target.as_os_str().as_bytes().to_vec()
                    };
                    #[cfg(not(unix))]
                    let content = target.to_string_lossy().as_bytes().to_vec();
                    entries.push(SnapshotEntry::new(relative, 0o120000, content)?);
                } else if metadata.is_dir() {
                    pending.push((path, relative));
                } else if metadata.is_file() {
                    let content = std::fs::read(&path).map_err(|error| SnapshotError::Io {
                        path: path.display().to_string(),
                        message: error.to_string(),
                    })?;
                    let mode = file_mode(&metadata);
                    entries.push(SnapshotEntry::new(relative, mode, content)?);
                } else {
                    return Err(SnapshotError::UnsupportedEntry(relative));
                }
            }
        }
        Self::new(entries)
    }

    /// Return the identity covering every declared entry.
    pub fn identity(&self) -> &SnapshotIdentity {
        &self.identity
    }

    /// Look up a full entry by repository-relative path.
    pub fn entry(&self, path: &str) -> Option<&SnapshotEntry> {
        self.entries.get(path)
    }

    /// Iterate over all entries in stable path order.
    pub fn entries(&self) -> impl Iterator<Item = &SnapshotEntry> {
        self.entries.values()
    }

    /// Number of declared entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the snapshot contains no declared entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn is_ignored(path: &str, ignored_paths: &[String]) -> bool {
    ignored_paths.iter().any(|ignored| {
        path == ignored
            || path
                .strip_prefix(ignored)
                .is_some_and(|tail| tail.starts_with('/'))
    })
}

#[cfg(unix)]
fn file_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 == 0 {
        0o100644
    } else {
        0o100755
    }
}

#[cfg(not(unix))]
fn file_mode(_metadata: &std::fs::Metadata) -> u32 {
    0o100644
}

fn validate_path(path: &str) -> Result<(), SnapshotError> {
    let bytes = path.as_bytes();
    let invalid = path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        || path
            .split('/')
            .any(|part| part == ".." || part == "." || part.is_empty())
        || path.contains('\0');
    if invalid {
        return Err(SnapshotError::InvalidPath(path.to_string()));
    }
    Ok(())
}

fn identity_of(entries: &BTreeMap<String, SnapshotEntry>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rulebound-snapshot-v1\0");
    for entry in entries.values() {
        hasher.update(&(entry.path.len() as u64).to_be_bytes());
        hasher.update(entry.path.as_bytes());
        hasher.update(&entry.mode.to_be_bytes());
        hasher.update(&(entry.content.len() as u64).to_be_bytes());
        hasher.update(&entry.content);
    }
    hasher.finalize().to_hex().to_string()
}
