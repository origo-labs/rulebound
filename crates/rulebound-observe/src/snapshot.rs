//! Immutable, caller supplied repository snapshots and their content identity.

use std::collections::BTreeMap;
use std::fmt;

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

    /// File mode supplied by the snapshot owner.
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
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => write!(f, "invalid repository-relative path: {path:?}"),
            Self::DuplicatePath(path) => write!(f, "duplicate snapshot path: {path:?}"),
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

fn validate_path(path: &str) -> Result<(), SnapshotError> {
    let invalid = path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path
            .split(['/', '\\'])
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
