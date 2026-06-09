//! Identifier and time primitives.

use serde::{Deserialize, Serialize};

/// Opaque session identifier. Backed by the Claude Code session id (a string),
/// so we never assume a particular format.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier for a single review hunk within a session's pending diff.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HunkId(pub String);

impl HunkId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// UTC timestamp as epoch milliseconds (chosen for stable ordering; see architecture).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }
    pub const fn as_millis(self) -> i64 {
        self.0
    }
}
