//! Authentication actions. Terminal methods are executed by the client, not ACP `authenticate`.

use std::process::{Command, Stdio};

use agent_client_protocol::schema::v1::AuthMethod;
use tern_sdk::{
    Session,
    keys::Key,
    ui::{self, PickerItem},
};

use crate::{bus::TermiosController, launch::AgentLaunch};

use super::{Msg, input::Action};

pub(super) fn picker(auth: &crate::model::AuthPrompt, selected: usize) -> impl Into<ui::Node<Msg>> {
    let mut picker = ui::picker()
        .key("auth")
        .title("Authenticate")
        .items(
            auth.methods
                .iter()
                .enumerate()
                .map(|(i, method)| {
                    let mut item = PickerItem::new(format!("method:{i}"), method.name());
                    item.detail = method.description().map(Into::into);
                    item
                })
                .collect::<Vec<_>>(),
        )
        .on_select(|id| Msg::AuthSelect(id.to_owned()))
        .on_activate(|id| Msg::AuthActivate(id.to_owned()));
    if auth.methods.get(selected).is_some() {
        picker = picker.selected(format!("method:{selected}"));
    }
    if let Some(message) = &auth.message {
        picker = picker.subtitle(message.clone());
    }
    picker
}

pub(super) fn choose(model: &crate::model::Model, index: usize) -> Action {
    match model.auth.as_ref().and_then(|auth| auth.methods.get(index)) {
        Some(AuthMethod::Agent(method)) => {
            Action::Command(crate::event::UiCmd::Authenticate(method.id.clone()))
        }
        Some(AuthMethod::Terminal(_)) => Action::AuthTerminal(index),
        _ => Action::None,
    }
}

pub(super) fn key(key: &Key, model: &mut crate::model::Model) -> Action {
    if key.is("ctrl+d") {
        return Action::Quit;
    }
    let Some(auth) = &model.auth else {
        return Action::None;
    };
    if key.is("up") {
        model.auth_selected = model.auth_selected.saturating_sub(1);
        return Action::Changed;
    }
    if key.is("down") {
        model.auth_selected = (model.auth_selected + 1).min(auth.methods.len().saturating_sub(1));
        return Action::Changed;
    }
    if key.is("enter") {
        return choose(model, model.auth_selected);
    }
    if !key.ctrl
        && !key.alt
        && !key.meta
        && let Ok(index) = key.name.parse::<usize>()
        && (1..=9).contains(&index)
    {
        return choose(model, index - 1);
    }
    Action::None
}

/// Run terminal authentication with the current pane suspended. A failing
/// child leaves the auth picker in place so the user can choose another method.
pub(super) fn terminal_auth(
    session: &mut Session<Msg>,
    surface: tern_sdk::Surface,
    method: &AuthMethod,
    launch: &AgentLaunch,
    controller: Option<&TermiosController>,
) -> anyhow::Result<bool> {
    let AuthMethod::Terminal(method) = method else {
        return Ok(false);
    };
    session.suspend(surface)?;
    let mut command = Command::new(&launch.command);
    command
        .args(&launch.args)
        .args(&method.args)
        .envs(&launch.env)
        .envs(&method.env);
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut run = || command.status();
    let outcome = match controller {
        Some(controller) => controller.with_cooked(run),
        None => run(),
    };
    // Even when spawning the child fails, the TSP pane must be recovered.
    session.resume(surface)?;
    match outcome {
        Ok(status) => Ok(status.success()),
        Err(error) => Err(error.into()),
    }
}
