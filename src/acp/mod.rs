//! The ACP client: spawns the agent and translates between ACP and
//! [`crate::event`].

use std::path::PathBuf;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::bus::EventTx;
use crate::event::UiCmd;
use crate::launch::AgentLaunch;

/// Spawns `launch`, initializes, starts a session in `cwd`, and serves until
/// [`UiCmd::Shutdown`] or the command channel closes.
pub async fn run(
    launch: AgentLaunch,
    cwd: PathBuf,
    events: EventTx,
    cmds: UnboundedReceiver<UiCmd>,
) -> anyhow::Result<()> {
    let _ = (launch, cwd, events, cmds);
    anyhow::bail!("acp::run is not wired yet")
}
