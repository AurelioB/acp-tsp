//! The TSP side: owns the tty, renders the model, routes input.

pub mod attach;
mod auth;
pub mod composer;
pub mod content;
pub mod elicit;
mod ext;
mod input;
pub mod palette;
mod pane;
pub mod permission;
pub mod prefs;
#[cfg(feature = "unstable_llm_providers")]
mod providers;
mod theme;
mod usage;
pub mod view;

use std::path::PathBuf;

use agent_client_protocol::schema::v1::{
    AuthMethodId, CreateElicitationResponse, ElicitationAction, PermissionOptionId,
    RequestPermissionOutcome, TerminalId,
};
use tern_sdk::{
    Input, Options, Session, SurfaceOptions,
    term::Terminal,
    wire::{self, Event, RevealAt, ScrollBy},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::bus::{EventRx, TermiosController, WakeFd, WakeTerm};
use crate::event::{BridgeEvent, UiCmd};
use crate::launch::AgentLaunch;
use crate::model::Model;
use composer::Composer;
use input::{Action, InputState};
use palette::{Choice, Kind, Palette};

/// Whether the connected terminal can render a TSP element kind.
pub(crate) fn supports(caps: &tern_sdk::Capabilities, kind: &str) -> bool {
    caps.has_kind(kind)
}

/// What the UI needs to start.
#[derive(Clone, Debug)]
pub struct UiOptions {
    /// The agent's display name.
    pub agent: String,
    /// The session's working directory.
    pub cwd: PathBuf,
    /// Agent executable and base environment for terminal authentication.
    pub launch: AgentLaunch,
}

/// How the UI ended.
#[derive(Debug, PartialEq, Eq)]
pub enum UiExit {
    /// The user quit; `resume` is the open session's id when the agent can reopen it.
    Quit { resume: Option<String> },
    /// The terminal does not speak TSP (not Tern, `TERN_TSP=0`, a multiplexer).
    NoTern,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Send(String),
    Edit(wire::Edit),
    EditPrompt(u64),
    ToggleFold(String, bool),
    Undo,
    Focus,
    Submit,
    Interrupt,
    UsageOpen,
    UsageRefresh,
    UsageClose,
    UsageTab(String),
    ConfigChip(String),
    ModelPicker,
    EffortPicker,
    Restart,
    Quit,
    Permission(u64, PermissionOptionId),
    PermissionDismiss(u64),
    Authenticate(AuthMethodId),
    PickerSelect(String),
    PickerActivate(String),
    PickerConfirm,
    PickerClose,
    AuthSelect(String),
    AuthActivate(String),
    AuthQuit,
    ConfigChange(Option<String>, serde_json::Value),
    #[cfg(feature = "unstable_llm_providers")]
    ProviderChange(Option<String>, serde_json::Value),
    #[cfg(feature = "unstable_llm_providers")]
    ProviderApply,
    AttachSelect(String),
    ElicitEdit(wire::Edit),
    ElicitChange(wire::Change),
    ElicitChoice(String, String),
    ElicitFocus(String),
    ElicitAccept(u64),
    ElicitDecline(u64),
    ElicitCancel(u64),
    KillTerminal(TerminalId),
    TerminalFocus(TerminalId),
    AccountDetails,
    GoalChange(serde_json::Value),
    GoalAction(String),
}

/// Runs the UI on the current thread until the user quits.
pub fn run(
    options: UiOptions,
    events: EventRx,
    wake: WakeFd,
    cmds: UnboundedSender<UiCmd>,
    on_connected: impl FnOnce(),
) -> anyhow::Result<UiExit> {
    let term = WakeTerm::open(wake)?;
    let controller = term.termios_controller();
    run_with_controller(
        term,
        options,
        events,
        cmds,
        Some(controller),
        on_connected,
        stop_job,
    )
}

/// Same session loop with an injectable terminal for scripted tests.
#[doc(hidden)]
pub fn run_with_terminal(
    term: impl Terminal + 'static,
    options: UiOptions,
    events: EventRx,
    cmds: UnboundedSender<UiCmd>,
    on_connected: impl FnOnce(),
) -> anyhow::Result<UiExit> {
    run_with_controller(term, options, events, cmds, None, on_connected, stop_job)
}
/// Scripted session with an injectable job-control stop instead of a real signal.
#[doc(hidden)]
pub fn run_with_terminal_stop(
    term: impl Terminal + 'static,
    options: UiOptions,
    events: EventRx,
    cmds: UnboundedSender<UiCmd>,
    on_connected: impl FnOnce(),
    stop: impl Fn() -> std::io::Result<()>,
) -> anyhow::Result<UiExit> {
    run_with_controller(term, options, events, cmds, None, on_connected, stop)
}

fn stop_job() -> std::io::Result<()> {
    // Stops acp-tsp's own process group, as the shell's Ctrl+Z would. The ACP SDK starts the
    // adapter in a process group of its own (observed: `npm exec` leads a separate pgid), so the
    // agent keeps running; its messages wait in the pipe until `fg`.
    rustix::process::kill_current_process_group(rustix::process::Signal::TSTP)
        .map_err(std::io::Error::from)
}

fn run_with_controller(
    term: impl Terminal + 'static,
    options: UiOptions,
    events: EventRx,
    cmds: UnboundedSender<UiCmd>,
    controller: Option<TermiosController>,
    on_connected: impl FnOnce(),
    stop: impl Fn() -> std::io::Result<()>,
) -> anyhow::Result<UiExit> {
    let term = pane::SharedTerm::new(term);
    let mut signals = pane::PaneSignals::new(&term);
    let Some(mut session) = Session::<Msg>::with_terminal(
        term,
        Options::new()
            .app("acp-tsp")
            .version(env!("CARGO_PKG_VERSION"))
            .feature(wire::feature::EDIT)
            .feature(wire::feature::UNDO)
            .feature(wire::feature::SEND)
            .feature(wire::feature::SCROLL),
    )?
    else {
        return Ok(UiExit::NoTern);
    };
    let mut sf = session.open(surface_options(&options))?;
    session.palette(sf, theme::palette())?;
    on_connected();
    let mut model = Model::default();
    let mut usage_history = crate::usage_history::Recorder::new();
    model.launch_name = options.agent.clone();
    let mut composer = Composer::default();
    let mut input_state = InputState::default();
    let mut palette: Option<Palette> = None;
    let mut config_open = false;
    let mut config_selected = 0;
    let mut form = elicit::FormState::default();
    let mut picker = attach::AttachPicker::default();
    let mut focused_terminal: Option<TerminalId> = None;
    let mut settled_entries = std::collections::HashSet::new();
    let mut prompt_cursor: Option<usize> = None;
    let mut gone_retried = false;
    let mut awaiting_adopt = false;
    // The `--resume` picker shown before any session exists, and whether the user already
    // asked for a session from it.
    let mut startup_picker = false;
    let mut opened_session = false;
    // Drain before the first frame too: startup notifications may already be queued.
    match events.drain() {
        Some(batch) => {
            for event in batch {
                model.apply(event);
            }
        }
        None => {
            model.restartable = !cmds.is_closed();
            model.apply(crate::event::BridgeEvent::AgentExited {
                reason: "Agent connection closed".into(),
            });
        }
    }
    if let Some(request) = model.oldest_elicitation() {
        form.init(request.id, &request.request);
    }
    if std::mem::take(&mut model.pick_session) {
        palette = Some(Palette::new(Kind::Sessions));
        startup_picker = cmds.send(UiCmd::ListSessions { cursor: None }).is_ok();
    }
    content::upload(&mut session, &mut model)?;
    session.render(
        sf,
        screen(
            &model,
            &composer,
            session.caps(),
            &palette,
            config_open,
            &form,
            &picker,
        ),
    )?;
    settle_finished(&mut session, sf, &model, &mut settled_entries)?;
    // After the `o`, so the program's title replaces the surface's.
    signals.sync(&model)?;
    usage_history.record(&model);
    if let Some(request) = model.oldest_elicitation() {
        if let Some(id) = elicit::first_field(&request.request) {
            session.focus(sf, Some(&id))?;
        }
    } else {
        session.focus(sf, Some(crate::ui::view::EDITOR_ID))?;
    }
    while let Some(input) = session.next(None)? {
        if !matches!(&input, Input::Event(Event::Gone(_))) {
            gone_retried = false;
            awaiting_adopt = false;
        }
        let mut restore_focus = false;
        let elicitation_before = form.id;
        let dialog_before = model.dialogs.front().is_some()
            || model.auth.is_some()
            || model.ext.goal_open
            || model.ext.account_details;
        let usage_visible = model.usage_report.open && !dialog_before;
        let composer_blocked = dialog_before
            || model.exited.is_some()
            || model.usage_report.open
            || (model.usage_report.fetching && model.usage_command().is_some())
            || config_open
            || palette.as_ref().is_some_and(|p| p.kind != Kind::Commands);
        let action = match input {
            Input::Key(key) => {
                // Printable keys are user content: log only named keys.
                if key.typed().is_none() {
                    tracing::debug!(name = %key.name, ctrl = key.ctrl, "key");
                }
                let scroll_by = if key.is("page_up") {
                    Some(ScrollBy::PageUp)
                } else if key.is("page_down") {
                    Some(ScrollBy::PageDown)
                } else if key.is("home") {
                    Some(ScrollBy::Start)
                } else if key.is("end") {
                    Some(ScrollBy::End)
                } else {
                    None
                };
                if key.is("ctrl+z")
                    && !dialog_before
                    && !picker.open
                    && !config_open
                    && palette.is_none()
                    && !usage_visible
                {
                    let id = session.surface_id(sf).to_owned();
                    if session.is_open(sf) {
                        session.close_surface(sf, true)?;
                    }
                    // Cooked mode lets the shell take control while acp-tsp is stopped; the
                    // adapter, in its own process group, keeps running (see `stop_job`).
                    let stopped = if let Some(controller) = controller.as_ref() {
                        controller.with_cooked(&stop)
                    } else {
                        stop()
                    };
                    if let Err(error) = stopped {
                        model.errors.push(format!("Could not suspend job: {error}"));
                    }
                    match session.open(surface_options(&options).id(id).adopt(true)) {
                        Ok(reopened) => {
                            sf = reopened;
                            awaiting_adopt = true;
                            session.palette(sf, theme::palette())?;
                            content::reset_uploads(&mut model);
                            settled_entries.clear();
                            restore_focus = true;
                            Action::Changed
                        }
                        Err(_) => Action::Quit,
                    }
                } else if let Some(by) = scroll_by
                    && let Some(id) = scroll_target(
                        ScrollContext {
                            model: &model,
                            palette: &palette,
                            config_open,
                            picker_open: picker.open,
                            focused_terminal: focused_terminal.as_ref(),
                            usage_visible,
                            dialog_open: dialog_before,
                            composer_empty: composer.text.is_empty(),
                        },
                        by,
                    )
                    && session.is_open(sf)
                {
                    session.scroll(sf, &id, by)?;
                    Action::None
                } else if scroll_by
                    .is_some_and(|by| matches!(by, ScrollBy::PageUp | ScrollBy::PageDown))
                    && !dialog_before
                    && !picker.open
                    && !config_open
                    && palette.is_none()
                    && !usage_visible
                    && model.exited.is_none()
                    && session.is_open(sf)
                {
                    reveal_prompt(
                        &mut session,
                        sf,
                        &model,
                        &mut prompt_cursor,
                        key.is("page_up"),
                    )?;
                    Action::None
                } else if usage_visible && key.is("escape") {
                    model.close_usage();
                    Action::Changed
                } else if usage_visible && key.is("r") {
                    crate::usage_history::refresh(&mut model.usage_report);
                    model.usage_report.pending = true;
                    Action::Changed
                } else if usage_visible && key.is("tab") {
                    model.usage_report.details = !model.usage_report.details;
                    Action::Changed
                } else if usage_visible
                    || (model.usage_report.fetching
                        && model.usage_command().is_some()
                        && !dialog_before
                        && !key.is("escape"))
                {
                    Action::None
                } else if (model.ext.goal_open || model.ext.account_details) && key.is("escape") {
                    model.ext.goal_open = false;
                    model.ext.account_details = false;
                    Action::Changed
                } else if model.ext.goal_open || model.ext.account_details {
                    Action::None
                } else if picker.open && model.dialogs.front().is_none() && model.auth.is_none() {
                    if key.is("escape") {
                        picker.open = false;
                        Action::Changed
                    } else if key.is("enter") || key.is("tab") {
                        picker.accept_selected(&mut composer);
                        Action::Changed
                    } else if key.is("up") {
                        picker.selected = picker.selected.saturating_sub(1);
                        Action::Changed
                    } else if key.is("down") {
                        picker.selected =
                            (picker.selected + 1).min(picker.matches().len().saturating_sub(1));
                        Action::Changed
                    } else {
                        let action = input_state.key(&key, &mut model, &mut composer);
                        picker.sync(&composer.text, composer.cursor);
                        picker.selected = 0;
                        action
                    }
                } else if let Some(request) = model.oldest_elicitation().cloned() {
                    if key.is("escape") {
                        Action::Command(UiCmd::Elicitation {
                            id: request.id,
                            response: CreateElicitationResponse::new(ElicitationAction::Cancel),
                        })
                    } else {
                        let choice_changed =
                            if let agent_client_protocol::schema::v1::ElicitationMode::Form(
                                form_request,
                            ) = &request.request.mode
                            {
                                let schema = &form_request.requested_schema;
                                let typing = form.focus.is_some();
                                let changed =
                                    form.text_key(schema, &key) || form.option_key(schema, &key);
                                if typing && form.focus.is_none() && session.is_open(sf) {
                                    session.focus(sf, None)?;
                                }
                                changed
                            } else {
                                false
                            };
                        if key.is("enter") {
                            form.accept(request.id, &request.request)
                                .map_or(Action::Changed, Action::Command)
                        } else if choice_changed {
                            Action::Changed
                        } else {
                            Action::None
                        }
                    }
                } else if model.dialogs.front().is_none() && model.auth.is_some() {
                    auth::key(&key, &mut model)
                } else if model.dialogs.front().is_none()
                    && let Some(active) = &mut palette
                {
                    if active.kind == Kind::Commands
                        && !key.is("escape")
                        && !key.is("up")
                        && !key.is("down")
                        && !key.is("enter")
                        && !key.is("tab")
                    {
                        let action = input_state.key(&key, &mut model, &mut composer);
                        if composer.text.starts_with('/')
                            && !composer.text.contains(char::is_whitespace)
                        {
                            active.query = composer.text[1..].to_owned();
                            active.selected = 0;
                        } else {
                            palette = None;
                        }
                        action
                    } else if key.is("enter")
                        && active.kind == Kind::Commands
                        && matches!(composer.text.as_str(), "/model" | "/usage")
                        && let Some(choice) = palette::typed_client_command(&composer.text, &model)
                    {
                        palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                    } else {
                        let choice = active.key(&key, &model);
                        palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                    }
                } else if model.dialogs.front().is_none() && config_open {
                    if key.is("escape") {
                        config_open = false;
                        Action::Changed
                    } else {
                        prefs::key(&key, &model, &mut config_selected)
                            .map_or(Action::Changed, Action::Command)
                    }
                } else if model.dialogs.front().is_none()
                    && model.auth.is_none()
                    && model.exited.is_none()
                    && key.is("shift+tab")
                {
                    model.next_mode().map_or(Action::None, Action::Command)
                } else if key.is("enter")
                    && !composer_blocked
                    && let Some(choice) = palette::typed_client_command(&composer.text, &model)
                {
                    composer = Composer::default();
                    palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                } else if !composer_blocked && composer.text.is_empty() && key.typed() == Some("/")
                {
                    composer.insert("/");
                    palette = Some(Palette::new(Kind::Commands));
                    Action::Changed
                } else if key.typed() == Some("@")
                    && !composer_blocked
                    && let Some(current) = &model.session
                {
                    composer.insert("@");
                    picker.open(&current.cwd);
                    picker.sync(&composer.text, composer.cursor);
                    Action::Changed
                } else if key.is("k")
                    && composer.text.is_empty()
                    && let Some(id) = focused_terminal.as_ref()
                    && model
                        .content
                        .terminals
                        .get(id)
                        .is_some_and(|state| !state.released && state.status.is_none())
                {
                    Action::Command(UiCmd::KillTerminal(id.clone()))
                } else {
                    input_state.key(&key, &mut model, &mut composer)
                }
            }
            Input::Msg(message, _) => match message {
                Msg::Send(text) if !composer_blocked => {
                    // Tern sends the text it saw; ignore a stale duplicate after local submit.
                    if text != composer.text {
                        Action::None
                    } else if let Some(choice) = palette::typed_client_command(&text, &model) {
                        composer = Composer::default();
                        palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                    } else {
                        input::submit(&mut model, &mut composer)
                    }
                }
                Msg::ModelPicker if !composer_blocked => {
                    palette = Some(Palette::new(Kind::Model));
                    Action::Changed
                }
                Msg::EffortPicker if !composer_blocked => {
                    palette = Some(Palette::new(Kind::Effort));
                    Action::Changed
                }
                Msg::ModelPicker | Msg::EffortPicker => Action::None,
                Msg::Submit if !composer_blocked => {
                    if let Some(choice) = palette::typed_client_command(&composer.text, &model) {
                        composer = Composer::default();
                        palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                    } else {
                        input::submit(&mut model, &mut composer)
                    }
                }
                Msg::Interrupt if matches!(model.turn, crate::model::Turn::Running { .. }) => {
                    model.cancel();
                    Action::Command(UiCmd::Cancel)
                }
                Msg::UsageOpen => Action::OpenUsage,
                Msg::UsageRefresh if model.usage_report.open => {
                    crate::usage_history::refresh(&mut model.usage_report);
                    model.usage_report.pending = true;
                    Action::Changed
                }
                Msg::UsageClose if model.usage_report.open => {
                    model.close_usage();
                    Action::Changed
                }
                Msg::UsageTab(tab) if model.usage_report.open => {
                    model.usage_report.details = tab == "details";
                    Action::Changed
                }
                Msg::ConfigChip(id) => prefs::active_chips(&model)
                    .into_iter()
                    .find(|(option, _, _)| option == &id)
                    .map_or(Action::None, |(_, _, cmd)| Action::Command(cmd)),
                Msg::UsageRefresh | Msg::UsageClose | Msg::UsageTab(_) => Action::None,
                Msg::Restart if model.exited.is_some() && model.restartable => {
                    model.exited = None;
                    model.session = None;
                    model.status = "Restarting".into();
                    Action::Command(UiCmd::Restart)
                }
                Msg::Quit => Action::Quit,
                Msg::Restart => Action::None,
                Msg::Submit | Msg::Interrupt => Action::None,
                Msg::Edit(edit) if !composer_blocked => {
                    if composer.edit(&edit) {
                        focused_terminal = None;
                        if composer.text.starts_with('/')
                            && !composer.text.contains(char::is_whitespace)
                        {
                            let commands =
                                palette.get_or_insert_with(|| Palette::new(Kind::Commands));
                            if commands.kind == Kind::Commands {
                                commands.query = composer.text[1..].to_owned();
                                commands.selected = 0;
                            }
                        } else if palette.as_ref().is_some_and(|p| p.kind == Kind::Commands) {
                            palette = None;
                        }
                        if picker.open {
                            picker.sync(&composer.text, composer.cursor);
                            picker.selected = 0;
                        } else if edit.text.contains('@')
                            && let Some(current) = &model.session
                        {
                            picker.open(&current.cwd);
                            picker.sync(&composer.text, composer.cursor);
                        }
                        Action::Changed
                    } else {
                        Action::None
                    }
                }
                Msg::EditPrompt(id) if !composer_blocked => {
                    if let Some(text) = model.transcript.iter().find_map(|entry| {
                        (entry.id == id).then_some(&entry.kind).and_then(|kind| {
                            if let crate::model::EntryKind::User { text, .. } = kind {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                    }) {
                        composer.replace(text);
                        focused_terminal = None;
                        palette = None;
                        picker.open = false;
                        if session.is_open(sf) {
                            session.focus(sf, Some(crate::ui::view::EDITOR_ID))?;
                        }
                        Action::Changed
                    } else {
                        Action::None
                    }
                }
                Msg::ToggleFold(key, collapsed) => {
                    let id = key
                        .strip_prefix("entry.")
                        .and_then(|tail| tail.split('.').next())
                        .and_then(|id| id.parse::<u64>().ok());
                    if id.is_some_and(|id| model.transcript.iter().any(|entry| entry.id == id)) {
                        model.folds.insert(key, collapsed);
                        Action::Changed
                    } else {
                        Action::None
                    }
                }
                Msg::Undo if !composer_blocked => {
                    if composer.undo() {
                        Action::Changed
                    } else {
                        Action::None
                    }
                }
                Msg::Focus if !composer_blocked => {
                    focused_terminal = None;
                    if session.is_open(sf) {
                        session.focus(sf, Some(crate::ui::view::EDITOR_ID))?;
                    }
                    Action::None
                }
                Msg::Send(_) | Msg::Edit(_) | Msg::EditPrompt(_) | Msg::Undo | Msg::Focus => {
                    Action::None
                }
                Msg::AttachSelect(path) if picker.open => {
                    let path = path.strip_prefix("layer.complete.0.list.").unwrap_or(&path);
                    if picker
                        .matches()
                        .iter()
                        .any(|(item, _)| item.to_string_lossy() == path)
                    {
                        picker.accept(&mut composer, std::path::Path::new(path));
                        Action::Changed
                    } else {
                        Action::None
                    }
                }
                Msg::AttachSelect(_) => Action::None,
                Msg::ElicitEdit(edit) => {
                    if let Some(request) = model.oldest_elicitation()
                        && let agent_client_protocol::schema::v1::ElicitationMode::Form(
                            form_request,
                        ) = &request.request.mode
                        && form.edit(&edit, &form_request.requested_schema)
                    {
                        Action::Changed
                    } else {
                        Action::None
                    }
                }
                Msg::ElicitChange(change) => {
                    if let Some(request) = model.oldest_elicitation()
                        && let agent_client_protocol::schema::v1::ElicitationMode::Form(
                            form_request,
                        ) = &request.request.mode
                        && form.change(&change, &form_request.requested_schema)
                    {
                        Action::Changed
                    } else {
                        Action::None
                    }
                }
                Msg::ElicitChoice(name, value) => {
                    if let Some(request) = model.oldest_elicitation()
                        && let agent_client_protocol::schema::v1::ElicitationMode::Form(
                            form_request,
                        ) = &request.request.mode
                        && form.choose(&form_request.requested_schema, &name, &value)
                    {
                        Action::Changed
                    } else {
                        Action::None
                    }
                }
                Msg::ElicitFocus(field) => {
                    if model.oldest_elicitation().is_some() {
                        form.focus_field(&field);
                        if session.is_open(sf) {
                            session.focus(
                                sf,
                                Some(&format!("layer.elicitation.form.{field}.field.{field}")),
                            )?;
                        }
                        Action::Changed
                    } else {
                        Action::None
                    }
                }
                Msg::ElicitAccept(id) => model
                    .oldest_elicitation()
                    .cloned()
                    .filter(|p| p.id == id)
                    .and_then(|p| form.accept(id, &p.request))
                    .map_or(Action::Changed, Action::Command),
                Msg::ElicitDecline(id)
                    if model.oldest_elicitation().is_some_and(|p| p.id == id) =>
                {
                    Action::Command(UiCmd::Elicitation {
                        id,
                        response: CreateElicitationResponse::new(ElicitationAction::Decline),
                    })
                }
                Msg::ElicitCancel(id) if model.oldest_elicitation().is_some_and(|p| p.id == id) => {
                    Action::Command(UiCmd::Elicitation {
                        id,
                        response: CreateElicitationResponse::new(ElicitationAction::Cancel),
                    })
                }
                Msg::ElicitDecline(_) | Msg::ElicitCancel(_) => Action::None,
                Msg::TerminalFocus(id) => {
                    focused_terminal = Some(id);
                    Action::None
                }
                Msg::KillTerminal(id) => {
                    if model
                        .content
                        .terminals
                        .get(&id)
                        .is_some_and(|terminal| !terminal.released && terminal.status.is_none())
                    {
                        Action::Command(UiCmd::KillTerminal(id))
                    } else {
                        Action::None
                    }
                }
                Msg::Permission(id, option) => {
                    if model.oldest_permission().is_some_and(|p| p.id == id) {
                        Action::Command(input::permission(id, option))
                    } else {
                        Action::None
                    }
                }
                Msg::PermissionDismiss(id)
                    if model.oldest_permission().is_some_and(|p| p.id == id) =>
                {
                    if let Some(option) = model.reject_once() {
                        Action::Command(input::permission(id, option))
                    } else {
                        Action::Command(UiCmd::Permission {
                            id,
                            outcome: RequestPermissionOutcome::Cancelled,
                        })
                    }
                }
                Msg::PermissionDismiss(_) => Action::None,
                Msg::Authenticate(id) => {
                    if model
                        .auth
                        .as_ref()
                        .is_some_and(|auth| auth.methods.iter().any(|method| method.id() == &id))
                    {
                        Action::Command(UiCmd::Authenticate(id))
                    } else {
                        Action::None
                    }
                }
                Msg::PickerConfirm if palette.is_some() => {
                    let choice = palette
                        .as_mut()
                        .expect("picker checked")
                        .activate(&model, false);
                    palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                }
                Msg::PickerClose if palette.is_some() => {
                    palette = None;
                    Action::Changed
                }
                Msg::PickerSelect(id) if palette.is_some() => {
                    let choice = palette
                        .as_mut()
                        .expect("picker checked")
                        .select(&id, &model, false);
                    palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                }
                Msg::PickerActivate(id) if palette.is_some() => {
                    let choice = palette
                        .as_mut()
                        .expect("picker checked")
                        .select(&id, &model, true);
                    palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                }
                Msg::AuthSelect(id) => {
                    if let Some(index) = id
                        .strip_prefix("method:")
                        .and_then(|s| s.parse::<usize>().ok())
                        && model
                            .auth
                            .as_ref()
                            .is_some_and(|auth| index < auth.methods.len())
                    {
                        model.auth_selected = index;
                        Action::Changed
                    } else {
                        Action::None
                    }
                }
                Msg::AuthActivate(id) => id
                    .strip_prefix("method:")
                    .and_then(|s| s.parse::<usize>().ok())
                    .map_or(Action::None, |index| auth::choose(&model, index)),
                Msg::AuthQuit if model.auth.is_some() => Action::Quit,
                Msg::AccountDetails => {
                    model.ext.account_details = !model.ext.account_details;
                    Action::Changed
                }
                Msg::GoalChange(value) if model.ext.goal_open => {
                    if let Some(objective) = value.as_str() {
                        model.ext.goal_draft = objective.into();
                    }
                    Action::Changed
                }
                Msg::GoalAction(action) if model.ext.goal_open => {
                    let Some(session) = &model.session else {
                        continue;
                    };
                    if !model
                        .ext
                        .goal
                        .as_ref()
                        .is_some_and(|goal| goal.actions.contains(&action))
                        || (action == "set" && model.ext.goal_draft.trim().is_empty())
                    {
                        Action::None
                    } else {
                        let mut params =
                            serde_json::json!({"sessionId": session.id, "action": action});
                        if action == "set" {
                            params["objective"] = serde_json::json!(model.ext.goal_draft.trim());
                        }
                        model.ext.goal_open = false;
                        Action::Command(UiCmd::Extension {
                            method: "_session/goal".into(),
                            params,
                        })
                    }
                }
                Msg::ConfigChange(id, value) if config_open => {
                    prefs::change(&model, id.as_deref(), &value)
                        .map_or(Action::None, Action::Command)
                }
                #[cfg(feature = "unstable_llm_providers")]
                Msg::ProviderChange(id, value) => {
                    if let Some(editor) = palette
                        .as_mut()
                        .and_then(|picker| picker.provider_editor.as_mut())
                    {
                        editor
                            .change(id.as_deref(), &value)
                            .map_or(Action::Changed, Action::Command)
                    } else {
                        Action::None
                    }
                }
                #[cfg(feature = "unstable_llm_providers")]
                Msg::ProviderApply => {
                    if let Some(editor) = palette
                        .as_mut()
                        .and_then(|picker| picker.provider_editor.as_mut())
                    {
                        editor.apply().map_or(Action::Changed, Action::Command)
                    } else {
                        Action::None
                    }
                }
                Msg::AuthQuit
                | Msg::PickerConfirm
                | Msg::PickerClose
                | Msg::PickerSelect(_)
                | Msg::PickerActivate(_)
                | Msg::ConfigChange(_, _)
                | Msg::GoalChange(_)
                | Msg::GoalAction(_) => Action::None,
            },
            Input::Event(Event::Error(error)) => {
                tracing::warn!(message = %error.msg, "TSP rejected operation");
                Action::None
            }
            // Tern also reports settled transcript rows it evicted under its retention budget
            // (`gone` with node ids); only the surface's own id means the session view is gone.
            Input::Event(Event::Gone(gone))
                if gone.ids.iter().any(|id| id == session.surface_id(sf)) =>
            {
                if gone_retried {
                    tracing::warn!("TSP reopened surface is gone again");
                    Action::Quit
                } else {
                    gone_retried = true;
                    tracing::warn!(adopt = awaiting_adopt, "TSP surface gone; reopening");
                    match session
                        .open(surface_options(&options).id(session.surface_id(sf).to_owned()))
                    {
                        Ok(reopened) => {
                            sf = reopened;
                            if session.palette(sf, theme::palette()).is_err() {
                                Action::Quit
                            } else {
                                content::reset_uploads(&mut model);
                                settled_entries.clear();
                                restore_focus = true;
                                awaiting_adopt = false;
                                Action::Changed
                            }
                        }
                        Err(_) => Action::Quit,
                    }
                }
            }
            Input::Event(Event::Gone(gone)) => {
                tracing::debug!(nodes = gone.ids.len(), "Tern evicted settled rows");
                Action::None
            }
            Input::Event(event) => {
                tracing::debug!(ev = event.name(), "unhandled TSP event");
                Action::None
            }
        };
        let mut changed = matches!(action, Action::Changed);
        match action {
            Action::Command(cmd) => {
                if matches!(cmd, UiCmd::Prompt(_) | UiCmd::Steer(_)) {
                    prompt_cursor = None;
                }
                opened_session |= matches!(
                    cmd,
                    UiCmd::NewSession | UiCmd::LoadSession(_) | UiCmd::ResumeSession(_)
                );
                // The ACP side answers silently; the dialog closes on the user's answer.
                if let UiCmd::Permission { id, .. } | UiCmd::Elicitation { id, .. } = &cmd {
                    model.apply(crate::event::BridgeEvent::Resolved(*id));
                }
                if cmds.send(cmd).is_err() {
                    model.restartable = false;
                    model.apply(crate::event::BridgeEvent::AgentExited {
                        reason: "Agent controller unavailable; restart is not possible".into(),
                    });
                }
                changed = true;
            }
            Action::OpenGoal => {
                palette = None;
                model.ext.goal_open = true;
                changed = true;
            }
            Action::OpenUsage => {
                palette = None;
                crate::usage_history::refresh(&mut model.usage_report);
                config_open = false;
                model.usage_report.open = true;
                model.usage_report.details = false;
                if model.usage_report.checked.is_none() && !model.usage_report.fetching {
                    model.usage_report.pending = true;
                }
                changed = true;
            }
            Action::AuthTerminal(index) => {
                let outcome = model
                    .auth
                    .as_ref()
                    .and_then(|auth| auth.methods.get(index))
                    .map(|method| {
                        auth::terminal_auth(
                            &mut session,
                            sf,
                            method,
                            &options.launch,
                            controller.as_ref(),
                        )
                    });
                match outcome {
                    Some(Ok(true)) => {
                        model.auth = None;
                        model.status = "Reconnecting".into();
                        if cmds.send(UiCmd::RetrySession).is_err() {
                            model.errors.push("Agent controller unavailable".into());
                        }
                    }
                    Some(Ok(false)) => model.errors.push("Authentication failed".into()),
                    Some(Err(error)) => {
                        model.errors.push(format!("Authentication failed: {error}"))
                    }
                    None => {}
                }
                changed = true;
            }
            Action::Quit => break,
            _ => {}
        }
        match events.drain() {
            Some(batch) => {
                changed |= !batch.is_empty();
                for event in batch {
                    if matches!(event, BridgeEvent::SessionStarting(_)) {
                        palette = None;
                        config_open = false;
                        composer = Composer::default();
                        picker = attach::AttachPicker::default();
                        focused_terminal = None;
                        settled_entries.clear();
                    }
                    model.apply(event);
                }
            }
            None if model.exited.is_none() => {
                model.restartable = !cmds.is_closed();
                model.apply(crate::event::BridgeEvent::AgentExited {
                    reason: "Agent connection closed".into(),
                });
                changed = true;
            }
            None => {}
        }
        if !model.dialogs.is_empty() || model.auth.is_some() {
            picker.open = false;
            palette = None;
            config_open = false;
        }
        if let Some(request) = model.oldest_elicitation() {
            form.init(request.id, &request.request);
        } else {
            form = elicit::FormState::default();
        }
        if session.is_open(sf)
            && dialog_before
            && model.dialogs.front().is_none()
            && model.auth.is_none()
            && !picker.open
        {
            session.focus(sf, Some(crate::ui::view::EDITOR_ID))?;
        }
        if std::mem::take(&mut model.pick_session) {
            palette = Some(Palette::new(Kind::Sessions));
            startup_picker = true;
            if cmds.send(UiCmd::ListSessions { cursor: None }).is_err() {
                startup_picker = false;
            }
            changed = true;
        } else if startup_picker
            && palette
                .as_ref()
                .is_none_or(|open| open.kind != Kind::Sessions)
        {
            // Closed without a pick (a pick has already sent its own session command).
            startup_picker = false;
            if model.session.is_none() && !opened_session {
                let _ = cmds.send(UiCmd::NewSession);
            }
        }
        if model.usage_report.pending && model.usage_request().is_none() {
            model.usage_report.pending = false;
            model.usage_report.error =
                Some("This agent does not offer a local usage command".into());
            changed = true;
        }
        if model.usage_report.pending
            && !model.usage_report.fetching
            && model.session.is_some()
            && model.exited.is_none()
            && (model.usage_command().is_none()
                || (matches!(model.turn, crate::model::Turn::Idle) && model.queued == 0))
        {
            model.begin_usage();
            if let Some(command) = model.usage_request()
                && cmds.send(command).is_err()
            {
                model.usage_report.fetching = false;
                model.usage_report.error = Some("Agent controller unavailable".into());
                model.apply(BridgeEvent::AgentExited {
                    reason: "Agent controller unavailable".into(),
                });
            }
            changed = true;
        }
        if !supports(session.caps(), "prefs")
            && settings_open(&mut model, &mut config_open, &mut palette)
        {
            model
                .notices
                .push("This Tern version cannot show settings sheets".into());
            changed = true;
        }
        if changed && session.is_open(sf) {
            content::upload(&mut session, &mut model)?;
            session.render(
                sf,
                screen(
                    &model,
                    &composer,
                    session.caps(),
                    &palette,
                    config_open,
                    &form,
                    &picker,
                ),
            )?;
            settle_finished(&mut session, sf, &model, &mut settled_entries)?;
            signals.sync(&model)?;
            usage_history.record(&model);
            if restore_focus || form.id != elicitation_before {
                if let Some(request) = model.oldest_elicitation()
                    && let Some(id) = elicit::first_field(&request.request)
                {
                    session.focus(sf, Some(&id))?;
                } else if restore_focus && model.ext.goal_open {
                    session.focus(sf, Some("layer.goal"))?;
                } else if restore_focus && model.ext.account_details {
                    session.focus(sf, Some("layer.account.details"))?;
                } else if restore_focus && picker.open {
                    session.focus(sf, Some("layer.complete.0.list"))?;
                } else if restore_focus && model.usage_report.open {
                    session.focus(sf, Some("layer.usage.body.head.tabs"))?;
                } else if restore_focus && config_open {
                    session.focus(sf, Some("layer.config"))?;
                } else if restore_focus && let Some(active) = palette.as_ref() {
                    #[cfg(feature = "unstable_llm_providers")]
                    let target = if active.provider_editor.is_some() {
                        "layer.provider"
                    } else if active.kind == Kind::Commands {
                        "layer.complete.0.list"
                    } else {
                        "layer.palette"
                    };
                    #[cfg(not(feature = "unstable_llm_providers"))]
                    let target = if active.kind == Kind::Commands {
                        "layer.complete.0.list"
                    } else {
                        "layer.palette"
                    };
                    session.focus(sf, Some(target))?;
                } else if restore_focus
                    && let Some(terminal) = focused_terminal.as_ref()
                    && let Some(output) = scroll_target(
                        ScrollContext {
                            model: &model,
                            palette: &palette,
                            config_open,
                            picker_open: picker.open,
                            focused_terminal: Some(terminal),
                            usage_visible: false,
                            dialog_open: false,
                            composer_empty: composer.text.is_empty(),
                        },
                        ScrollBy::PageUp,
                    )
                    && let Some(card) = output.strip_suffix(".output")
                {
                    session.focus(sf, Some(card))?;
                } else if restore_focus && model.dialogs.front().is_some() {
                    session.focus(sf, None)?;
                } else {
                    session.focus(sf, Some(crate::ui::view::EDITOR_ID))?;
                }
            }
        }
    }
    let _ = cmds.send(UiCmd::Shutdown);
    signals.clear()?;
    session.close()?;
    Ok(UiExit::Quit {
        resume: model
            .session
            .as_ref()
            .filter(|_| model.can_reopen_sessions())
            .map(|session| session.id.to_string()),
    })
}

/// Let Tern move finished transcript rows out of the live diff set.
fn settle_finished(
    session: &mut Session<Msg>,
    sf: tern_sdk::Surface,
    model: &Model,
    settled: &mut std::collections::HashSet<u64>,
) -> anyhow::Result<()> {
    for entry in &model.transcript {
        let finished = match &entry.kind {
            crate::model::EntryKind::Tool { call, cancelled } => {
                *cancelled
                    || matches!(
                        call.status,
                        agent_client_protocol::schema::v1::ToolCallStatus::Completed
                            | agent_client_protocol::schema::v1::ToolCallStatus::Failed
                    )
            }
            crate::model::EntryKind::Agent { .. } => !model.is_streaming(entry),
            crate::model::EntryKind::Thought { .. } => entry.finished_at.is_some(),
            _ => false,
        };
        if finished && settled.insert(entry.id) {
            session.settle(sf, &format!("main.entry.{}", entry.id))?;
        }
    }
    Ok(())
}
fn surface_options(options: &UiOptions) -> SurfaceOptions {
    SurfaceOptions::inline()
        .title(options.agent.clone())
        .role("omp.session")
}

fn settings_open(model: &mut Model, config_open: &mut bool, palette: &mut Option<Palette>) -> bool {
    let open = std::mem::take(config_open) | std::mem::take(&mut model.ext.goal_open);
    #[cfg(feature = "unstable_llm_providers")]
    if let Some(picker) = palette.as_mut() {
        return open | picker.provider_editor.take().is_some();
    }
    #[cfg(not(feature = "unstable_llm_providers"))]
    let _ = palette;
    open
}

struct ScrollContext<'a> {
    model: &'a Model,
    palette: &'a Option<Palette>,
    config_open: bool,
    picker_open: bool,
    focused_terminal: Option<&'a TerminalId>,
    usage_visible: bool,
    dialog_open: bool,
    composer_empty: bool,
}

fn scroll_target(context: ScrollContext<'_>, by: ScrollBy) -> Option<String> {
    if context.dialog_open {
        return None;
    }
    if context.usage_visible {
        return context
            .model
            .usage_report
            .details
            .then(|| "layer.usage.body.raw".into());
    }
    if context.picker_open {
        return matches!(by, ScrollBy::PageUp | ScrollBy::PageDown)
            .then(|| "layer.complete.0.list".into());
    }
    if let Some(palette) = context.palette {
        if !matches!(by, ScrollBy::PageUp | ScrollBy::PageDown) {
            return None; // Home/End edit the picker query (or command composer).
        }
        #[cfg(feature = "unstable_llm_providers")]
        if palette.provider_editor.is_some() {
            return Some("layer.provider".into());
        }
        return Some(
            if palette.kind == Kind::Commands {
                "layer.complete.0.list"
            } else {
                "layer.palette"
            }
            .into(),
        );
    }
    if context.config_open {
        return matches!(by, ScrollBy::PageUp | ScrollBy::PageDown).then(|| "layer.config".into());
    }
    let id = context.focused_terminal?;
    if !context.composer_empty {
        return None; // Home/End belong to the editor while text is present.
    }
    for entry in context.model.transcript.iter().rev() {
        let crate::model::EntryKind::Tool { call, .. } = &entry.kind else {
            continue;
        };
        for (index, content) in call.content.iter().enumerate() {
            if let agent_client_protocol::schema::v1::ToolCallContent::Terminal(terminal) = content
                && terminal.terminal_id == *id
            {
                return Some(format!("main.entry.{}.{index}.output", entry.id));
            }
        }
    }
    None
}

fn reveal_prompt(
    session: &mut Session<Msg>,
    sf: tern_sdk::Surface,
    model: &Model,
    cursor: &mut Option<usize>,
    previous: bool,
) -> anyhow::Result<()> {
    let prompts = || {
        model
            .transcript
            .iter()
            .filter(|entry| matches!(entry.kind, crate::model::EntryKind::User { .. }))
    };
    if previous {
        let index = cursor
            .unwrap_or_else(|| prompts().count())
            .saturating_sub(1);
        if let Some(entry) = prompts().nth(index) {
            session.reveal(sf, &format!("main.entry.{}", entry.id), RevealAt::Start)?;
            *cursor = Some(index);
        }
    } else if let Some(index) = *cursor
        && let Some(entry) = prompts().nth(index + 1)
    {
        session.reveal(sf, &format!("main.entry.{}", entry.id), RevealAt::Start)?;
        *cursor = Some(index + 1);
    } else if let Some(entry) = model.transcript.last() {
        // Tern currently treats `end` as `nearest`; reveal at the viewport edge.
        session.reveal(sf, &format!("main.entry.{}", entry.id), RevealAt::Nearest)?;
        *cursor = None;
    }
    Ok(())
}

fn screen(
    model: &Model,
    composer: &Composer,
    caps: &tern_sdk::Capabilities,
    palette: &Option<Palette>,
    config_open: bool,
    form: &elicit::FormState,
    picker: &attach::AttachPicker,
) -> tern_sdk::ui::View<Msg> {
    let mut view = view::view_with(model, composer, caps, form, Some(picker));
    if model.dialogs.front().is_none()
        && model.auth.is_none()
        && !picker.open
        && !model.ext.account_details
        && !model.ext.goal_open
    {
        if model.usage_report.open {
            view = view.layer(usage::render(model, caps));
        } else if let Some(palette) = palette {
            view = view.layer(view::layer(palette.render(model)));
        } else if config_open {
            view = view.layer(view::layer(prefs::render(model)));
        }
    }
    view
}

fn palette_choice(
    choice: Choice,
    palette: &mut Option<Palette>,
    config_open: &mut bool,
    composer: &mut Composer,
) -> Action {
    match choice {
        Choice::Changed => Action::Changed,
        Choice::Close => {
            *palette = None;
            Action::Changed
        }
        Choice::Insert(text) => {
            *palette = None;
            if composer.text.starts_with('/') {
                for _ in 0..composer.text.chars().count() {
                    composer.backspace();
                }
            }
            composer.insert(&text);
            Action::Changed
        }
        Choice::Command(cmd) => {
            if composer.text.starts_with('/') {
                for _ in 0..composer.text.chars().count() {
                    composer.backspace();
                }
            }
            let keep = matches!(cmd, UiCmd::ListSessions { .. } | UiCmd::DeleteSession(_));
            #[cfg(feature = "unstable_llm_providers")]
            let keep = keep || matches!(cmd, UiCmd::SetProvider(_) | UiCmd::DisableProvider(_));
            if !keep {
                *palette = None;
            }
            Action::Command(cmd)
        }
        Choice::Open(kind, cmd) => {
            if composer.text.starts_with('/') {
                for _ in 0..composer.text.chars().count() {
                    composer.backspace();
                }
            }
            *palette = Some(Palette::new(kind));
            cmd.map_or(Action::Changed, Action::Command)
        }
        Choice::Goal => {
            if composer.text.starts_with('/') {
                for _ in 0..composer.text.chars().count() {
                    composer.backspace();
                }
            }
            *palette = None;
            Action::OpenGoal
        }
        Choice::Usage => {
            if composer.text.starts_with('/') {
                for _ in 0..composer.text.chars().count() {
                    composer.backspace();
                }
            }
            *palette = None;
            Action::OpenUsage
        }
        Choice::Prefs => {
            if composer.text.starts_with('/') {
                for _ in 0..composer.text.chars().count() {
                    composer.backspace();
                }
            }
            *palette = None;
            *config_open = true;
            Action::Changed
        }
        Choice::Quit => Action::Quit,
    }
}
