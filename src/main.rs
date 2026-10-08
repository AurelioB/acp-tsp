use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ClientCapabilities, FileSystemCapabilities, Implementation, InitializeRequest,
    SessionNotification,
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, ConnectionTo};
use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use acp_tsp::{
    acp, bus,
    config::Config,
    event::UiCmd,
    launch::{AgentLaunch, SessionOptions},
    tern_settings, ui,
};

#[derive(Debug, Parser)]
#[command(
    name = "acp-tsp",
    version,
    about = "Run ACP agents natively in a Tern terminal pane"
)]
struct Cli {
    #[command(subcommand)]
    action: Option<Action>,
    /// Select a configured or built-in agent.
    #[arg(long, global = true)]
    agent: Option<String>,
    /// Session working directory.
    #[arg(long, global = true)]
    cwd: Option<PathBuf>,
    /// Agent command and arguments (takes precedence over --agent).
    #[arg(last = true)]
    command: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// List built-in and configured agents.
    Agents,
    /// Configure Tern to launch acp-tsp in agent blocks.
    SetupTern {
        #[arg(long)]
        settings: Option<PathBuf>,
        #[arg(long)]
        print: bool,
        /// Make new Tern blocks agent blocks (not changed by default).
        #[arg(long)]
        new_blocks_agent: bool,
    },
    /// Initialize an agent and print its ACP InitializeResponse.
    Probe,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.action {
        Some(Action::Agents) => list_agents(),
        Some(Action::SetupTern {
            settings,
            print,
            new_blocks_agent,
        }) => setup_tern(
            cli.agent.as_deref(),
            settings.as_deref(),
            print,
            new_blocks_agent,
        ),
        Some(Action::Probe) => probe(cli.agent.as_deref()),
        None => run_session(cli),
    }
}

fn run_session(cli: Cli) -> anyhow::Result<()> {
    let _log_guard = init_logging()?;
    let cwd = canonical_cwd(cli.cwd.as_deref())?;
    let launch = if cli.command.is_empty() {
        Config::load()?.resolve(cli.agent.as_deref())?
    } else {
        let command = &cli.command[0];
        let name = Path::new(command)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(command);
        AgentLaunch {
            name: name.to_owned(),
            command: command.clone(),
            args: cli.command[1..].to_vec(),
            env: Default::default(),
        }
    };
    let (waker, wake_fd) = bus::wake_pair()?;
    let (events_tx, events_rx) = bus::event_channel(waker);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let acp_task = runtime.spawn(acp::run(
        launch.clone(),
        SessionOptions::new(cwd.clone()),
        events_tx,
        cmd_rx,
    ));
    let ui_result = ui::run(
        ui::UiOptions {
            agent: launch.name.clone(),
            cwd,
            launch,
        },
        events_rx,
        wake_fd,
        cmd_tx.clone(),
    );
    let _ = cmd_tx.send(UiCmd::Shutdown);
    let finished =
        runtime.block_on(async { tokio::time::timeout(Duration::from_secs(2), acp_task).await });
    if finished.is_err() {
        tracing::warn!("ACP task did not stop within two seconds");
    }
    runtime.shutdown_timeout(Duration::from_secs(2));
    match ui_result? {
        ui::UiExit::Quit => Ok(()),
        ui::UiExit::NoTern => {
            eprintln!(
                "acp-tsp needs Tern (a terminal that speaks TSP). Run it in a Tern agent block; check TERN_TSP is not disabled."
            );
            std::process::exit(2)
        }
    }
}

fn list_agents() -> anyhow::Result<()> {
    let config = Config::load()?;
    let default = config.default_agent.as_deref().unwrap_or("claude");
    for name in config.known_agents() {
        match config.resolve(Some(&name)) {
            Ok(launch) => println!(
                "{}{}: {} {}",
                name,
                if name == default { " (default)" } else { "" },
                launch.command,
                launch.args.join(" ")
            ),
            Err(error) if name == "agy" && error.to_string().contains("not installed") => println!(
                "{}{}: not installed",
                name,
                if name == default { " (default)" } else { "" }
            ),
            Err(error) => println!("{name}: {error}"),
        }
    }
    Ok(())
}

fn setup_tern(
    agent: Option<&str>,
    settings: Option<&Path>,
    print: bool,
    new_blocks_agent: bool,
) -> anyhow::Result<()> {
    let executable =
        std::env::current_exe().context("could not determine acp-tsp executable path")?;
    if print {
        return tern_settings::print_snippet(&executable, agent, new_blocks_agent);
    }
    let path = tern_settings::settings_path(settings)?;
    let changed = tern_settings::update_settings(&path, &executable, agent, new_blocks_agent)?;
    if changed.is_empty() {
        println!("{} is already configured; no changes made", path.display());
    } else {
        println!("Updated {}: {}", path.display(), changed.join(", "));
    }
    Ok(())
}

fn canonical_cwd(cwd: Option<&Path>) -> anyhow::Result<PathBuf> {
    let requested = cwd.map(Path::to_owned).unwrap_or(std::env::current_dir()?);
    let path = std::fs::canonicalize(&requested).with_context(|| {
        format!(
            "could not resolve working directory {}",
            requested.display()
        )
    })?;
    if !path.is_absolute() {
        bail!("ACP requires an absolute working directory");
    }
    if !path.is_dir() {
        bail!("working directory {} is not a directory", path.display());
    }
    Ok(path)
}

fn init_logging() -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/state")
        });
    let directory = state.join("acp-tsp");
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("could not create log directory {}", directory.display()))?;
    let appender = tracing_appender::rolling::never(directory, "acp-tsp.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::builder()
        .parse_lossy(log_directives(std::env::var("ACP_TSP_LOG").ok().as_deref()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .map_err(|error| anyhow::anyhow!("could not initialize file logging: {error}"))?;
    Ok(guard)
}

/// A bare level (`debug`) applies to acp-tsp only: dependency debug logs dump raw ACP
/// payloads, which carry account e-mails. Full directives (`agent_client_protocol=debug`)
/// are taken as written.
fn log_directives(env: Option<&str>) -> String {
    match env.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) if v.contains('=') || v.contains(',') => v.to_owned(),
        Some(level) => format!("warn,acp_tsp={level}"),
        None => "warn,acp_tsp=info".to_owned(),
    }
}

fn probe(agent_name: Option<&str>) -> anyhow::Result<()> {
    let launch = Config::load()?.resolve(agent_name)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let agent = AcpAgent::new(
            AcpAgentConfig::new(launch.command)
                .args(launch.args)
                .envs(launch.env),
        );
        agent_client_protocol::Client
            .builder()
            .on_receive_notification(
                async |_notification: SessionNotification, _cx| Ok(()),
                agent_client_protocol::on_receive_notification!(),
            )
            .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
                let capabilities = ClientCapabilities::new().fs(FileSystemCapabilities::new()
                    .read_text_file(true)
                    .write_text_file(true));
                let request = InitializeRequest::new(ProtocolVersion::V1)
                    .client_info(Implementation::new("acp-tsp", env!("CARGO_PKG_VERSION")))
                    .client_capabilities(capabilities);
                let response = connection.send_request(request).block_task().await?;
                println!("{}", serde_json::to_string_pretty(&response)?);
                Ok(())
            })
            .await?;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}
