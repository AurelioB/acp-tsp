//! The contract between the ACP side ([`crate::acp`]) and the UI side
//! ([`crate::model`] + [`crate::ui`]).
//!
//! ACP → UI travels as [`BridgeEvent`] over [`crate::bus::EventTx`];
//! UI → ACP travels as [`UiCmd`] over a tokio unbounded channel. ACP wire
//! types are passed through unchanged so the model sees exactly what the agent
//! sent.

use std::path::PathBuf;

use agent_client_protocol::schema::v1::{
    AuthMethod, AuthMethodId, ContentBlock, CreateElicitationRequest, CreateElicitationResponse,
    ElicitationId, InitializeResponse, PromptResponse, RequestPermissionOutcome,
    RequestPermissionRequest, SessionConfigId, SessionConfigOption, SessionConfigOptionValue,
    SessionId, SessionInfo, SessionModeId, SessionModeState, SessionUpdate, TerminalExitStatus,
    TerminalId,
};
#[cfg(feature = "unstable_llm_providers")]
use agent_client_protocol::schema::v1::{
    DisableProviderRequest, ListProvidersResponse, SetProviderRequest,
};

/// Identifies one agent → client request awaiting a user decision. Allocated
/// by the ACP side, unique per process.
pub type PendingId = u64;

/// How a session came to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStartKind {
    /// `session/new`.
    New,
    /// `session/load`: the agent replays history as `Update`s before
    /// `SessionStarted`.
    Load,
    /// `session/resume`: no replay.
    Resume,
    /// `session/fork` (unstable): a new session id branched from the current one.
    Fork,
}

/// ACP side → UI.
#[derive(Debug)]
pub enum BridgeEvent {
    /// Progress text while starting (spawning, initializing, creating the
    /// session). Replaced by the next status or by `SessionStarted`.
    Status(String),
    /// `initialize` succeeded.
    Initialized(Box<InitializeResponse>),
    /// The agent needs authentication before a session can start. The UI shows
    /// a picker and answers with [`UiCmd::Authenticate`] (agent methods) or
    /// runs a terminal method itself and answers [`UiCmd::RetrySession`].
    AuthRequired {
        /// The agent's advertised methods (agent and, when the client
        /// advertised `auth.terminal`, terminal methods).
        methods: Vec<AuthMethod>,
        /// The agent's error message, when it gave one.
        message: Option<String>,
    },
    /// A session request is in flight and will replace the current session:
    /// the UI clears the transcript. For `Load`, replayed history arrives as
    /// `Update`s between this and `SessionStarted`.
    SessionStarting(SessionStartKind),
    /// A session is ready for prompts.
    SessionStarted {
        /// Its id.
        session_id: SessionId,
        /// How it started.
        kind: SessionStartKind,
        /// Its working directory.
        cwd: PathBuf,
        /// Extra workspace roots granted to it.
        additional_dirs: Vec<PathBuf>,
        /// Legacy modes, when the agent sent them.
        modes: Option<SessionModeState>,
        /// Config options, when the agent sent them.
        config_options: Option<Vec<SessionConfigOption>>,
    },
    /// The current session was closed (`session/close`); no session is active
    /// until the next `SessionStarted`.
    SessionClosed,
    /// A page of `session/list`.
    SessionList {
        /// Sessions on this page.
        sessions: Vec<SessionInfo>,
        /// Cursor for [`UiCmd::ListSessions`] to fetch the next page.
        next_cursor: Option<String>,
        /// Whether this is the first page (replace) or a later one (append).
        first_page: bool,
    },
    /// `session/delete` succeeded.
    SessionDeleted(SessionId),
    /// The full config option list after `session/set_config_option`.
    ConfigOptions(Vec<SessionConfigOption>),
    /// The current mode after a successful `session/set_mode` (agents may
    /// also send `current_mode_update`).
    ModeChanged(SessionModeId),
    /// A `session/update` for the active session.
    Update(Box<SessionUpdate>),
    /// `session/request_permission`: answer with [`UiCmd::Permission`].
    Permission {
        /// Correlates the answer.
        id: PendingId,
        /// The request as received.
        request: Box<RequestPermissionRequest>,
    },
    /// `elicitation/create`: answer with [`UiCmd::Elicitation`].
    Elicitation {
        /// Correlates the answer.
        id: PendingId,
        /// The request as received.
        request: Box<CreateElicitationRequest>,
    },
    /// `elicitation/complete`: the URL-mode elicitation finished out of band.
    ElicitationComplete(ElicitationId),
    /// A pending request (permission or elicitation) was answered without the
    /// user (turn cancelled, `$/cancel_request`, agent exited): drop its
    /// dialog.
    Resolved(PendingId),
    /// A client-side terminal (`terminal/*`) changed.
    Terminal(TerminalEvent),
    /// The prompt turn finished (the `session/prompt` response).
    TurnEnded(Result<Box<PromptResponse>, String>),
    /// `providers/list` result (unstable).
    #[cfg(feature = "unstable_llm_providers")]
    Providers(Box<ListProvidersResponse>),
    /// `logout` succeeded; the agent needs authentication again.
    LoggedOut,
    /// A `_`-prefixed extension notification from the agent (`_auth/status_update`,
    /// `_claude/rateLimit`, ...). The params may hold account data: show, never log.
    Extension {
        /// The method name, including the leading `_`.
        method: String,
        /// Its params as received.
        params: serde_json::Value,
    },
    /// The answer to [`UiCmd::Extension`].
    ExtensionResult {
        /// The method that was called.
        method: String,
        /// Its result, or the JSON-RPC error message.
        result: Result<serde_json::Value, String>,
    },
    /// A non-fatal error to show (failed request, bad fs path, ...).
    Error(String),
    /// The agent process exited or the connection closed. The UI offers
    /// restart ([`UiCmd::Restart`]) or quit.
    AgentExited {
        /// Why, for display (exit status and stderr tail when known).
        reason: String,
    },
}

/// A client-side terminal's lifecycle, for live display in tool calls that
/// embed it (`ToolCallContent::Terminal`).
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    /// `terminal/create` spawned a process.
    Created {
        /// The id given to the agent.
        id: TerminalId,
        /// The command line, for display.
        command: String,
        /// Its working directory.
        cwd: PathBuf,
    },
    /// New output (already decoded lossily as UTF-8).
    Output {
        /// Which terminal.
        id: TerminalId,
        /// The bytes since the last event.
        chunk: String,
        /// Whether earlier output was dropped to honour `outputByteLimit`.
        truncated: bool,
    },
    /// The process exited.
    Exited {
        /// Which terminal.
        id: TerminalId,
        /// Exit code / signal.
        status: TerminalExitStatus,
    },
    /// `terminal/release`: the id is no longer valid (keep the last output on
    /// screen).
    Released(TerminalId),
}

/// UI → ACP side.
#[derive(Debug)]
pub enum UiCmd {
    /// Send a prompt. When a turn is running, the ACP side queues it and sends
    /// it after the turn ends.
    Prompt(Vec<ContentBlock>),
    /// Steer the running turn with more input (`_session/steering`) when the agent advertises
    /// `_meta.steering.supported`; otherwise (or when no turn runs) the same as `Prompt`.
    Steer(Vec<ContentBlock>),
    /// `session/cancel` the running turn. The ACP side also answers every
    /// pending permission request with `cancelled` and every pending
    /// elicitation with `cancel` (spec MUST) and emits
    /// [`BridgeEvent::Resolved`] for each.
    Cancel,
    /// The user's answer to [`BridgeEvent::Permission`].
    Permission {
        /// From the event.
        id: PendingId,
        /// The chosen outcome.
        outcome: RequestPermissionOutcome,
    },
    /// The user's answer to [`BridgeEvent::Elicitation`].
    Elicitation {
        /// From the event.
        id: PendingId,
        /// Accept (with content), decline or cancel.
        response: CreateElicitationResponse,
    },
    /// `authenticate` with this agent-type method, then retry session
    /// creation.
    Authenticate(AuthMethodId),
    /// Retry session creation after the UI ran a terminal auth method.
    RetrySession,
    /// `logout` (when `agentCapabilities.auth.logout`).
    Logout,
    /// Start a fresh session (`session/new`) in the same cwd.
    NewSession,
    /// `session/list` from `cursor` (None: first page), filtered to the cwd.
    ListSessions {
        /// From a previous [`BridgeEvent::SessionList`].
        cursor: Option<String>,
    },
    /// `session/load` (replays history).
    LoadSession(SessionId),
    /// `session/resume` (no replay).
    ResumeSession(SessionId),
    /// `session/close` the current session.
    CloseSession,
    /// `session/delete` a stored session.
    DeleteSession(SessionId),
    /// `session/fork` the current session (unstable).
    ForkSession,
    /// `session/set_mode`.
    SetMode(SessionModeId),
    /// `session/set_config_option`.
    SetConfigOption {
        /// Which option.
        config_id: SessionConfigId,
        /// Its new value.
        value: SessionConfigOptionValue,
    },
    /// `terminal/kill` from the user (the agent still releases it).
    KillTerminal(TerminalId),
    /// `providers/list` (unstable).
    #[cfg(feature = "unstable_llm_providers")]
    ListProviders,
    /// `providers/set` (unstable).
    #[cfg(feature = "unstable_llm_providers")]
    SetProvider(Box<SetProviderRequest>),
    /// `providers/disable` (unstable).
    #[cfg(feature = "unstable_llm_providers")]
    DisableProvider(Box<DisableProviderRequest>),
    /// A `_`-prefixed extension request (Codex `_session/goal`, `_session/async_task/stop`);
    /// answered with [`BridgeEvent::ExtensionResult`].
    Extension {
        /// The method name, including the leading `_`.
        method: String,
        /// Its params.
        params: serde_json::Value,
    },
    /// Kill and respawn the agent, starting a new session.
    Restart,
    /// Shut down the agent and stop.
    Shutdown,
}
