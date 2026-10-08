//! How to start an agent process.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A resolved agent launch: what to spawn and with which environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLaunch {
    /// Display name (`claude`, `codex`, `agy`, or a config key).
    pub name: String,
    /// The executable (resolved through `PATH` when not absolute).
    pub command: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}
