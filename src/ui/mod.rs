//! The TSP side: owns the tty, renders the model, routes input.

use std::path::PathBuf;

use tokio::sync::mpsc::UnboundedSender;

use crate::bus::{EventRx, WakeFd};
use crate::event::UiCmd;

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

/// Runs the UI on the current thread until the user quits.
pub fn run(
    options: UiOptions,
    events: EventRx,
    wake: WakeFd,
    cmds: UnboundedSender<UiCmd>,
) -> anyhow::Result<UiExit> {
    let _ = (options, events, wake, cmds);
    anyhow::bail!("ui::run is not wired yet")
}
