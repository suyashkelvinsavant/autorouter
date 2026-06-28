//! Strongly-typed identifiers used across the workspace.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub Uuid);

impl RequestId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}
impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}
impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}
impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for RequestId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}
impl From<Uuid> for SessionId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}
