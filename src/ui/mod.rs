//! The TSP side: owns the tty, renders the model, routes input.

mod auth;
pub mod composer;
mod input;
pub mod palette;
pub mod prefs;
pub mod view;

use std::path::PathBuf;

use agent_client_protocol::schema::v1::{AuthMethodId, PermissionOptionId};
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
    session.render(
        sf,
        screen(&model, &composer, session.caps(), &palette, config_open),
    )?;
    session.focus(sf, Some("dock.ed"))?;
    while let Some(input) = session.next(None)? {
        let dialog_before = model.oldest_permission().is_some() || model.auth.is_some();
        let composer_blocked =
            dialog_before || model.exited.is_some() || palette.is_some() || config_open;
        let action =
            match input {
                Input::Key(key) => {
                    // Printable keys are user content: log only named keys.
                    if key.typed().is_none() {
                        tracing::debug!(name = %key.name, ctrl = key.ctrl, "key");
                    }
                    if model.oldest_permission().is_none() && model.auth.is_some() {
                        auth::key(&key, &mut model)
                    } else if model.oldest_permission().is_none() && let Some(active) = &mut palette {
                        let choice = active.key(&key, &model);
                        palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                    } else if model.oldest_permission().is_none() && config_open {
                        if key.is("escape") {
                            config_open = false;
                            Action::Changed
                        } else {
                            prefs::key(&key, &model, &mut config_selected)
                                .map_or(Action::Changed, Action::Command)
                        }
                    } else if model.oldest_permission().is_none() && model.exited.is_none()
                        && key.is("tab") && key.shift {
                        model.next_mode().map_or(Action::None, Action::Command)
                    } else if model.oldest_permission().is_none() && model.auth.is_none()
                        && model.exited.is_none() && composer.text.is_empty() && key.typed() == Some("/") {
                        palette = Some(Palette::new(Kind::Commands));
                        Action::Changed
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
                            if composer.text == "/" {
                                composer = Composer::default();
                                palette = Some(Palette::new(Kind::Commands));
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
                        session.focus(sf, Some("dock.ed"))?;
                        Action::None
                    }
                    Msg::Send(_) | Msg::Edit(_) | Msg::Undo | Msg::Focus => Action::None,
                    Msg::Permission(id, option) => {
                        if model.oldest_permission().is_some_and(|p| p.id == id) {
                            Action::Command(input::permission(id, option))
                        } else {
                            Action::None
                        }
                    }
                    Msg::Authenticate(id) => {
                        if model.auth.as_ref().is_some_and(|auth| {
                            auth.methods.iter().any(|method| matches!(method, agent_client_protocol::schema::v1::AuthMethod::Agent(agent) if agent.id == id))
                        }) {
                            Action::Command(UiCmd::Authenticate(id))
                        } else {
                            Action::None
                        }
                    }
                    Msg::PickerSelect(id) if palette.is_some() => {
                        let choice = palette.as_mut().expect("picker checked").select(&id, &model, false);
                        palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                    }
                    Msg::PickerActivate(id) if palette.is_some() => {
                        let choice = palette.as_mut().expect("picker checked").select(&id, &model, true);
                        palette_choice(choice, &mut palette, &mut config_open, &mut composer)
                    }
                    Msg::AuthSelect(id) => {
                        if let Some(index) = id.strip_prefix("method:").and_then(|s| s.parse::<usize>().ok())
                            && model.auth.as_ref().is_some_and(|auth| index < auth.methods.len())
                        {
                            model.auth_selected = index;
                            Action::Changed
                        } else { Action::None }
                    }
                    Msg::AuthActivate(id) => id.strip_prefix("method:")
                        .and_then(|s| s.parse::<usize>().ok())
                        .map_or(Action::None, |index| auth::choose(&model, index)),
                    Msg::ConfigChange(id, value) if config_open =>
                        prefs::change(&model, id.as_deref(), &value)
                            .map_or(Action::None, Action::Command),
                    Msg::PickerSelect(_) | Msg::PickerActivate(_) | Msg::ConfigChange(_, _) => Action::None,
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
                if let UiCmd::Permission { id, .. } = &cmd {
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
        if dialog_before && model.oldest_permission().is_none() && model.auth.is_none() {
            session.focus(sf, Some("dock.ed"))?;
        }
        if changed {
            session.render(
                sf,
                screen(&model, &composer, session.caps(), &palette, config_open),
            )?;
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
) -> tern_sdk::ui::View<Msg> {
    let mut view = view::view(model, composer, caps);
    if model.oldest_permission().is_none() && model.auth.is_none() {
        if let Some(palette) = palette {
            view = view.layer(palette.render(model));
        } else if config_open {
            view = view.layer(prefs::render(model));
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
            if !matches!(cmd, UiCmd::ListSessions { .. } | UiCmd::DeleteSession(_)) {
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
