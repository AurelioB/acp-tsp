use std::path::PathBuf;

use acp_tsp::{acp, bus, launch::AgentLaunch, ui};

fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let launch = AgentLaunch {
        name: "claude".into(),
        command: "npx".into(),
        args: vec![
            "-y".into(),
            "@agentclientprotocol/claude-agent-acp@0.87.0".into(),
        ],
        env: Default::default(),
    };
    let (waker, wake_fd) = bus::wake_pair()?;
    let (events_tx, events_rx) = bus::event_channel(waker);
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let runtime = tokio::runtime::Runtime::new()?;
    let acp_task = runtime.spawn(acp::run(launch.clone(), cwd.clone(), events_tx, cmd_rx));
    let exit = ui::run(
        ui::UiOptions {
            agent: launch.name,
            cwd: PathBuf::from(&cwd),
        },
        events_rx,
        wake_fd,
        cmd_tx,
    )?;
    runtime.block_on(async { acp_task.abort() });
    if exit == ui::UiExit::NoTern {
        eprintln!("acp-tsp needs Tern (a terminal that speaks TSP)");
        std::process::exit(2);
    }
    Ok(())
}
