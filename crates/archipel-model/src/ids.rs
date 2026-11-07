//! Identifiers: object ids, repo ids, project ids and repo paths.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A Git object id (SHA-1 or SHA-256), stored inline without allocation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid {
    len: u8,
    bytes: [u8; 32],
}

/// Error returned when parsing an [`Oid`] from text fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid object id: {0:?}")]
pub struct InvalidOid(pub String);

impl Oid {
    /// Builds an id from raw bytes (20 for SHA-1, 32 for SHA-256).
    #[must_use]
    pub fn from_bytes(raw: &[u8]) -> Option<Self> {
        if raw.len() != 20 && raw.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 32];
        bytes[..raw.len()].copy_from_slice(raw);
        #[allow(clippy::cast_possible_truncation)] // length is 20 or 32
        Some(Self {
            len: raw.len() as u8,
            bytes,
        })
    }

    /// Parses a full 40 or 64 character hexadecimal id.
    pub fn from_hex(hex: &str) -> Result<Self, InvalidOid> {
        let hex = hex.trim();
        let err = || InvalidOid(hex.to_owned());
        if hex.len() != 40 && hex.len() != 64 {
            return Err(err());
        }
        let mut bytes = [0u8; 32];
        for (i, &[hi, lo]) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let hi = hex_val(hi).ok_or_else(err)?;
            let lo = hex_val(lo).ok_or_else(err)?;
            bytes[i] = (hi << 4) | lo;
        }
        #[allow(clippy::cast_possible_truncation)] // 20 or 32
        Ok(Self {
            len: (hex.len() / 2) as u8,
            bytes,
        })
    }

    /// The raw bytes of the id.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// Full lowercase hexadecimal form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(usize::from(self.len) * 2);
        for b in self.as_bytes() {
            s.push(char::from(HEX[usize::from(b >> 4)]));
            s.push(char::from(HEX[usize::from(b & 0xf)]));
        }
        s
    }

    /// Abbreviated hexadecimal form (`len` characters, at most the full length).
    #[must_use]
    pub fn short(&self, len: usize) -> String {
        let mut hex = self.to_hex();
        hex.truncate(len);
        hex
    }

    /// Whether this is the all-zero "null" id.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.as_bytes().iter().all(|b| *b == 0)
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self.short(12))
    }
}

impl std::str::FromStr for Oid {
    type Err = InvalidOid;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}

impl Serialize for Oid {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Oid {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

/// Stable identifier of a repository inside the application (derived from its path).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct RepoId(pub u64);

impl RepoId {
    /// Derives a stable id from a repository path (FNV-1a over the normalized path).
    ///
    /// Normalization lowercases on Windows and macOS (case-insensitive file
    /// systems) and unifies separators, so the same repo always gets the same id.
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        let text = path.to_string_lossy().replace('\\', "/");
        let text = text.trim_end_matches('/');
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.bytes() {
            let byte = if cfg!(any(windows, target_os = "macos")) {
                byte.to_ascii_lowercase()
            } else {
                byte
            };
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        Self(hash)
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// Identifier of a project in the user registry.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ProjectId(pub String);

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Absolute path of a repository's working tree (or of a bare repository).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct RepoPath(PathBuf);

impl RepoPath {
    /// Wraps a path. The path should be absolute.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    /// The wrapped path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// The id derived from this path.
    #[must_use]
    pub fn id(&self) -> RepoId {
        RepoId::from_path(&self.0)
    }

    /// Last path component, used as the default display name.
    #[must_use]
    pub fn name(&self) -> String {
        self.0.file_name().map_or_else(
            || self.0.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }
}

impl AsRef<Path> for RepoPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oid_hex_round_trip() {
        let hex = "0123456789abcdef0123456789abcdef01234567";
        let oid = Oid::from_hex(hex).unwrap();
        assert_eq!(oid.to_hex(), hex);
        assert_eq!(oid.as_bytes().len(), 20);
        assert_eq!(oid.short(7), "0123456");
        assert_eq!(Oid::from_hex(&hex.to_uppercase()).unwrap(), oid);
    }

    #[test]
    fn oid_sha256() {
        let hex = "ab".repeat(32);
        let oid = Oid::from_hex(&hex).unwrap();
        assert_eq!(oid.as_bytes().len(), 32);
        assert_eq!(oid.to_hex(), hex);
    }

    #[test]
    fn oid_rejects_garbage() {
        assert!(Oid::from_hex("xyz").is_err());
        assert!(Oid::from_hex(&"g".repeat(40)).is_err());
        assert!(Oid::from_bytes(&[0; 3]).is_none());
    }

    #[test]
    fn oid_serde() {
        let oid = Oid::from_bytes(&[7; 20]).unwrap();
        let json = serde_json::to_string(&oid).unwrap();
        assert_eq!(serde_json::from_str::<Oid>(&json).unwrap(), oid);
    }

    #[test]
    fn repo_id_ignores_trailing_separator() {
        assert_eq!(
            RepoId::from_path(Path::new("/a/b/")),
            RepoId::from_path(Path::new("/a/b"))
        );
        assert_ne!(
            RepoId::from_path(Path::new("/a/b")),
            RepoId::from_path(Path::new("/a/c"))
        );
    }

    #[test]
    fn repo_id_folds_case_where_the_file_system_does() {
        let same =
            RepoId::from_path(Path::new("/Work/App")) == RepoId::from_path(Path::new("/work/app"));
        assert_eq!(same, cfg!(any(windows, target_os = "macos")));
    }
}
