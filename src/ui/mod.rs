//! The TSP side: owns the tty, renders the model, routes input.

pub mod attach;
mod auth;
pub mod composer;
pub mod content;
pub mod elicit;
mod input;
pub mod palette;
pub mod prefs;
#[cfg(feature = "unstable_llm_providers")]
mod providers;
pub mod view;

use std::path::PathBuf;

use agent_client_protocol::schema::v1::{
    AuthMethodId, CreateElicitationResponse, ElicitationAction, PermissionOptionId, TerminalId,
};
use tern_sdk::{
    Input, Options, Session, SurfaceOptions,
    term::Terminal,
    wire::{self, Event},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::bus::{EventRx, TermiosController, WakeFd, WakeTerm};
use crate::event::{BridgeEvent, UiCmd};
use crate::launch::AgentLaunch;
use crate::model::Model;
use composer::Composer;
use input::{Action, InputState};
use palette::{Choice, Kind, Palette};

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
    /// The user quit.
    Quit,
    /// The terminal does not speak TSP (not Tern, `TERN_TSP=0`, a multiplexer).
    NoTern,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Send(String),
    Edit(wire::Edit),
    Undo,
    Focus,
    Permission(u64, PermissionOptionId),
    Authenticate(AuthMethodId),
    PickerSelect(String),
    PickerActivate(String),
    AuthSelect(String),
    AuthActivate(String),
    ConfigChange(Option<String>, serde_json::Value),
    #[cfg(feature = "unstable_llm_providers")]
    ProviderChange(Option<String>, serde_json::Value),
    #[cfg(feature = "unstable_llm_providers")]
    ProviderApply,
    AttachSelect(String),
    ElicitEdit(wire::Edit),
    ElicitChange(wire::Change),
    ElicitFocus(String),
    ElicitAccept(u64),
    ElicitDecline(u64),
    ElicitCancel(u64),
    KillTerminal(TerminalId),
    TerminalFocus(TerminalId),
}

/// Runs the UI on the current thread until the user quits.
pub fn run(
    options: UiOptions,
    events: EventRx,
    wake: WakeFd,
    cmds: UnboundedSender<UiCmd>,
) -> anyhow::Result<UiExit> {
    let term = WakeTerm::open(wake)?;
    let controller = term.termios_controller();
    run_with_controller(term, options, events, cmds, Some(controller))
}

/// Same session loop with an injectable terminal for scripted tests.
#[doc(hidden)]
pub fn run_with_terminal(
    term: impl Terminal + 'static,
    options: UiOptions,
    events: EventRx,
    cmds: UnboundedSender<UiCmd>,
) -> anyhow::Result<UiExit> {
    run_with_controller(term, options, events, cmds, None)
}

fn run_with_controller(
    term: impl Terminal + 'static,
    options: UiOptions,
    events: EventRx,
    cmds: UnboundedSender<UiCmd>,
    controller: Option<TermiosController>,
) -> anyhow::Result<UiExit> {
    let Some(mut session) = Session::<Msg>::with_terminal(
        term,
        Options::new()
            .app("acp-tsp")
            .version(env!("CARGO_PKG_VERSION"))
            .feature(wire::feature::EDIT)
            .feature(wire::feature::UNDO)
            .feature(wire::feature::SEND),
    )?
    else {
        return Ok(UiExit::NoTern);
    };
    let sf = session.open(
        SurfaceOptions::inline()
            .title(options.agent)
            .role("acp-tsp.session"),
    )?;
    let mut model = Model::default();
    let mut composer = Composer::default();
    let mut input_state = InputState::default();
    let mut palette: Option<Palette> = None;
    let mut config_open = false;
    let mut config_selected = 0;
    let mut form = elicit::FormState::default();
    let mut picker = attach::AttachPicker::default();
    let mut focused_terminal: Option<TerminalId> = None;
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
    if let Some(request) = model.oldest_elicitation() {
        if let Some(id) = elicit::first_field(&request.request) {
            session.focus(sf, Some(&id))?;
        }
    } else {
        session.focus(sf, Some("dock.ed"))?;
    }
    while let Some(input) = session.next(None)? {
        let elicitation_before = form.id;
        let dialog_before = model.dialogs.front().is_some() || model.auth.is_some();
        let composer_blocked = dialog_before
            || model.exited.is_some()
            || picker.open
            || palette.is_some()
            || config_open;
        let action = match input {
            Input::Key(key) => {
                // Printable keys are user content: log only named keys.
                if key.typed().is_none() {
                    tracing::debug!(name = %key.name, ctrl = key.ctrl, "key");
                }
                if picker.open && model.dialogs.front().is_none() && model.auth.is_none() {
                    if key.is("escape") {
                        picker.open = false;
                        session.focus(sf, Some("dock.ed"))?;
                        Action::Changed
                    } else if key.is("enter") {
                        if let Some(path) = picker.matches().get(picker.selected) {
                            composer.insert(&attach::mention(path));
                        }
                        picker.open = false;
                        session.focus(sf, Some("dock.ed"))?;
                        Action::Changed
                    } else if key.is("up") {
                        picker.selected = picker.selected.saturating_sub(1);
                        Action::Changed
                    } else if key.is("down") {
                        picker.selected =
                            (picker.selected + 1).min(picker.matches().len().saturating_sub(1));
                        Action::Changed
                    } else if key.is("backspace") {
                        picker.query.pop();
                        picker.selected = 0;
                        Action::Changed
                    } else if let Some(text) = key.typed() {
                        picker.query.push_str(text);
                        picker.selected = 0;
                        Action::Changed
                    } else {
                        Action::None
                    }
                } else if let Some(request) = model.oldest_elicitation().cloned() {
                    if key.is("escape") {
                        Action::Command(UiCmd::Elicitation {
                            id: request.id,
                            response: CreateElicitationResponse::new(ElicitationAction::Cancel),
                        })
                    } else if key.is("enter") {
                        form.accept(request.id, &request.request)
                            .map_or(Action::Changed, Action::Command)
                    } else {
                        Action::None
                    }
                } else if model.dialogs.front().is_none() && model.auth.is_some() {
                    auth::key(&key, &mut model)
                } else if model.dialogs.front().is_none()
                    && let Some(active) = &mut palette
                {
                    let choice = active.key(&key, &model);
                    palette_choice(choice, &mut palette, &mut config_open, &mut composer)
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
                    && key.is("tab")
                    && key.shift
                {
                    model.next_mode().map_or(Action::None, Action::Command)
                } else if !composer_blocked && composer.text.is_empty() && key.typed() == Some("/")
                {
                    palette = Some(Palette::new(Kind::Commands));
                    Action::Changed
                } else if key.typed() == Some("@")
                    && !composer_blocked
                    && let Some(current) = &model.session
                {
                    composer.insert("@");
                    picker.open(&current.cwd);
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
                    } else {
                        input::submit(&mut model, &mut composer)
                    }
                }
                Msg::Edit(edit) if !composer_blocked => {
                    if composer.edit(&edit) {
                        focused_terminal = None;
                        if composer.text == "/" {
                            composer = Composer::default();
                            palette = Some(Palette::new(Kind::Commands));
                        } else if edit.text.contains('@')
                            && let Some(current) = &model.session
                        {
                            picker.open(&current.cwd);
                        }
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
                    session.focus(sf, Some("dock.ed"))?;
                    Action::None
                }
                Msg::Send(_) | Msg::Edit(_) | Msg::Undo | Msg::Focus => Action::None,
                Msg::AttachSelect(path)
                    if picker.open
                        && picker
                            .matches()
                            .iter()
                            .any(|item| item.to_string_lossy() == path) =>
                {
                    composer.insert(&attach::mention(std::path::Path::new(&path)));
                    picker.open = false;
                    session.focus(sf, Some("dock.ed"))?;
                    Action::Changed
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
                Msg::ElicitFocus(field) => {
                    if model.oldest_elicitation().is_some() {
                        form.focus = Some(field.clone());
                        session.focus(
                            sf,
                            Some(&format!("layer.elicitation.form.{field}.field.{field}")),
                        )?;
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
                Msg::PickerSelect(_) | Msg::PickerActivate(_) | Msg::ConfigChange(_, _) => {
                    Action::None
                }
            },
            Input::Event(Event::Error(error)) => {
                tracing::warn!(message = %error.msg, "TSP rejected operation");
                Action::None
            }
            Input::Event(Event::Gone(_)) => {
                tracing::warn!("TSP surface gone");
                Action::Quit
            }
            Input::Event(event) => {
                tracing::debug!(ev = event.name(), "unhandled TSP event");
                Action::None
            }
        };
        let mut changed = matches!(action, Action::Changed);
        match action {
            Action::Command(cmd) => {
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
        if dialog_before && model.dialogs.front().is_none() && model.auth.is_none() && !picker.open
        {
            session.focus(sf, Some("dock.ed"))?;
        }
        if changed {
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
            if form.id != elicitation_before
                && let Some(request) = model.oldest_elicitation()
                && let Some(id) = elicit::first_field(&request.request)
            {
                session.focus(sf, Some(&id))?;
            }
        }
    }
    let _ = cmds.send(UiCmd::Shutdown);
    session.close()?;
    Ok(UiExit::Quit)
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
    if model.dialogs.front().is_none() && model.auth.is_none() && !picker.open {
        if let Some(palette) = palette {
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
            composer.insert(&text);
            Action::Changed
        }
        Choice::Command(cmd) => {
            let keep = matches!(cmd, UiCmd::ListSessions { .. } | UiCmd::DeleteSession(_));
            #[cfg(feature = "unstable_llm_providers")]
            let keep = keep || matches!(cmd, UiCmd::SetProvider(_) | UiCmd::DisableProvider(_));
            if !keep {
                *palette = None;
            }
            Action::Command(cmd)
        }
        Choice::Open(kind, cmd) => {
            *palette = Some(Palette::new(kind));
            cmd.map_or(Action::Changed, Action::Command)
        }
        Choice::Prefs => {
            *palette = None;
            *config_open = true;
            Action::Changed
        }
        Choice::Quit => Action::Quit,
    }
}
