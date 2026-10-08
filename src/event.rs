//! The contract between the ACP side ([`crate::acp`]) and the UI side
//! ([`crate::model`] + [`crate::ui`]).
//!
//! ACP → UI travels as [`BridgeEvent`] over [`crate::bus::EventTx`];
//! UI → ACP travels as [`UiCmd`] over a tokio unbounded channel. ACP wire
//! types are passed through unchanged so the model sees exactly what the agent
//! sent.

use agent_client_protocol::schema::v1::{
    AuthMethod, AuthMethodId, ContentBlock, InitializeResponse, RequestPermissionOutcome,
    RequestPermissionRequest, SessionConfigOption, SessionId, SessionModeState, SessionUpdate,
    StopReason,
};

/// Identifies one agent → client request awaiting a user decision. Allocated
/// by the ACP side, unique per process.
pub type PendingId = u64;

/// ACP side → UI.
#[derive(Debug)]
pub enum BridgeEvent {
    /// Progress text while starting (spawning, initializing, creating the
    /// session). Replaced by the next status or by `SessionStarted`.
    Status(String),
    /// `initialize` succeeded.
    Initialized(Box<InitializeResponse>),
    /// The agent needs authentication before a session can start. The UI shows
    /// a picker and answers with [`UiCmd::Authenticate`].
    AuthRequired {
        /// The agent's advertised methods.
        methods: Vec<AuthMethod>,
        /// The agent's error message, when it gave one.
        message: Option<String>,
    },
    /// A session is ready for prompts.
    SessionStarted {
        /// Its id.
        session_id: SessionId,
        /// Its working directory.
        cwd: std::path::PathBuf,
        /// Legacy modes, when the agent sent them.
        modes: Option<SessionModeState>,
        /// Config options, when the agent sent them.
        config_options: Option<Vec<SessionConfigOption>>,
    },
    /// A `session/update` for the active session.
    Update(Box<SessionUpdate>),
    /// `session/request_permission`: answer with [`UiCmd::Permission`].
    Permission {
        /// Correlates the answer.
        id: PendingId,
        /// The request as received.
        request: Box<RequestPermissionRequest>,
    },
    /// A pending request was answered without the user (turn cancelled, the
    /// agent sent `$/cancel_request`, or the agent exited): drop its dialog.
    Resolved(PendingId),
    /// The prompt turn finished (the `session/prompt` response).
    TurnEnded(Result<StopReason, String>),
    /// A non-fatal error to show (failed request, bad fs path, ...).
    Error(String),
    /// The agent process exited or the connection closed. The UI offers
    /// restart ([`UiCmd::Restart`]) or quit.
    AgentExited {
        /// Why, for display (exit status and stderr tail when known).
        reason: String,
    },
}

/// UI → ACP side.
#[derive(Debug)]
pub enum UiCmd {
    /// Send a prompt. When a turn is running, the ACP side queues it and sends
    /// it after the turn ends.
    Prompt(Vec<ContentBlock>),
    /// `session/cancel` the running turn. The ACP side also answers every
    /// pending permission request with `cancelled` (spec MUST) and emits
    /// [`BridgeEvent::Resolved`] for each.
    Cancel,
    /// The user's answer to [`BridgeEvent::Permission`].
    Permission {
        /// From the event.
        id: PendingId,
        /// The chosen outcome.
        outcome: RequestPermissionOutcome,
    },
    /// `authenticate` with this method, then retry session creation.
    Authenticate(AuthMethodId),
    /// Kill and respawn the agent, starting a new session.
    Restart,
    /// Shut down the agent and stop.
    Shutdown,
}
