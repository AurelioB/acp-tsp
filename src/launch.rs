//! How to start an agent process.

use std::collections::BTreeMap;
use std::path::PathBuf;

use agent_client_protocol::schema::v1::{McpServer, SessionId};

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

/// Which session to open first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum StartSession {
    /// `session/new`.
    #[default]
    New,
    /// `session/load` this id (falls back to `resume` when the agent lacks `loadSession`).
    Load(SessionId),
    /// `session/resume` this id.
    Resume(SessionId),
    /// The most recently updated session for the cwd (`session/list`), else a new one.
    Latest,
    /// Let the user pick from the cwd's sessions (`session/list`); closing the picker starts a
    /// new session.
    Pick,
}

/// Session setup shared by every session the ACP side creates.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionOptions {
    /// The primary workspace root (absolute, canonical).
    pub cwd: PathBuf,
    /// Extra workspace roots (`additionalDirectories`, absolute, canonical).
    pub additional_dirs: Vec<PathBuf>,
    /// MCP servers passed to session setup (filtered by the agent's `mcpCapabilities`).
    pub mcp_servers: Vec<McpServer>,
    /// The first session to open.
    pub start: StartSession,
    /// `_meta` sent with every session setup request (agent-specific options such as
    /// `claudeCode.options`), from the agent's config.
    pub session_meta: Option<serde_json::Map<String, serde_json::Value>>,
}

impl SessionOptions {
    /// A new session in `cwd` with nothing else.
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            additional_dirs: Vec::new(),
            mcp_servers: Vec::new(),
            start: StartSession::New,
            session_meta: None,
        }
    }
}
