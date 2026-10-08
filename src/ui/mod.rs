//! The TSP side: owns the tty, renders the model, routes input.

pub mod composer;
mod input;
pub mod view;

use std::path::PathBuf;

use agent_client_protocol::schema::v1::{AuthMethodId, PermissionOptionId};
use tern_sdk::{
    Input, Options, Session, SurfaceOptions,
    term::Terminal,
    wire::{self, Event},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::bus::{EventRx, WakeFd, WakeTerm};
use crate::event::UiCmd;
use crate::model::Model;
use composer::Composer;
use input::{Action, InputState};

/// What the UI needs to start.
#[derive(Clone, Debug)]
pub struct UiOptions {
    /// The agent's display name.
    pub agent: String,
    /// The session's working directory.
    pub cwd: PathBuf,
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
}

/// Runs the UI on the current thread until the user quits.
pub fn run(
    options: UiOptions,
    events: EventRx,
    wake: WakeFd,
    cmds: UnboundedSender<UiCmd>,
) -> anyhow::Result<UiExit> {
    run_with_terminal(WakeTerm::open(wake)?, options, events, cmds)
}

/// Same session loop with an injectable terminal for scripted tests.
#[doc(hidden)]
pub fn run_with_terminal(
    term: impl Terminal + 'static,
    options: UiOptions,
    events: EventRx,
    cmds: UnboundedSender<UiCmd>,
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
    session.render(sf, view::view(&model, &composer, session.caps()))?;
    session.focus(sf, Some("dock.ed"))?;
    while let Some(input) = session.next(None)? {
        let dialog_before = model.oldest_permission().is_some() || model.auth.is_some();
        let composer_blocked = dialog_before || model.exited.is_some();
        let action =
            match input {
                Input::Key(key) => input_state.key(&key, &mut model, &mut composer),
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
                            auth.methods.iter().any(|method| method.id() == &id)
                        }) {
                            Action::Command(UiCmd::Authenticate(id))
                        } else {
                            Action::None
                        }
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
                Input::Event(_) => Action::None,
            };
        let mut changed = matches!(action, Action::Changed);
        match action {
            Action::Command(cmd) => {
                if cmds.send(cmd).is_err() {
                    model.restartable = false;
                    model.apply(crate::event::BridgeEvent::AgentExited {
                        reason: "Agent controller unavailable; restart is not possible".into(),
                    });
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
            session.render(sf, view::view(&model, &composer, session.caps()))?;
        }
    }
    let _ = cmds.send(UiCmd::Shutdown);
    session.close()?;
    Ok(UiExit::Quit)
}
