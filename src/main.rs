use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{Implementation, InitializeRequest, SessionNotification};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Agent, ConnectionTo};
use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;

use acp_tsp::{
    acp, bus,
    config::Config,
    event::UiCmd,
    launch::{AgentLaunch, SessionOptions, StartSession},
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
    /// Resume a session by id, showing its history when the agent can replay it; without an
    /// id, pick one of this directory's sessions.
    #[arg(long, global = true, group = "session_start", num_args = 0..=1, value_name = "ID")]
    resume: Option<Option<String>>,
    /// Continue the most recently updated session in this directory.
    #[arg(long = "continue", global = true, group = "session_start")]
    continue_session: bool,
    /// Additional workspace root (repeatable, must be an existing directory).
    #[arg(long = "add-dir", global = true)]
    add_dirs: Vec<PathBuf>,
    /// Append redacted ACP JSONL lines to this file (also ACP_TSP_RECORD).
    #[arg(long, global = true)]
    record_acp: Option<PathBuf>,
    /// Agent command and arguments (takes precedence over --agent).
    #[arg(last = true)]
    command: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// List built-in and configured agents.
    Agents {
        #[command(subcommand)]
        command: Option<AgentCommand>,
    },
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
#[derive(Debug, Subcommand)]
enum AgentCommand {
    /// Install Antigravity's registry ACP server.
    Install {
        name: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Configure Antigravity ACP authentication.
    Auth {
        name: String,
        #[arg(long = "type")]
        auth_type: String,
    },
    /// Compare pinned adapters with registry versions.
    Update {
        name: Option<String>,
        #[arg(long)]
        write: bool,
    },
    /// List installed registry agents.
    List,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.action {
        Some(Action::Agents { command }) => match command {
            None | Some(AgentCommand::List) => list_agents(),
            Some(AgentCommand::Install { name, version, yes }) => acp_tsp::agents::command(
                "install",
                Some(&name),
                version.as_deref(),
                yes,
                false,
                None,
            ),
            Some(AgentCommand::Auth { name, auth_type }) => {
                acp_tsp::agents::command("auth", Some(&name), None, false, false, Some(&auth_type))
            }
            Some(AgentCommand::Update { name, write }) => {
                acp_tsp::agents::command("update", name.as_deref(), None, false, write, None)
            }
        },
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
    let config = Config::load()?;
    let launch = if cli.command.is_empty() {
        config.resolve(cli.agent.as_deref())?
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
    let resume_command = resume_command(&cli);
    let mut options = SessionOptions::new(cwd.clone());
    options.start = match (&cli.resume, cli.continue_session) {
        // `session/load` replays the history; it falls back to `session/resume`.
        (Some(Some(id)), false) => StartSession::Load(id.clone().into()),
        (Some(None), false) => StartSession::Pick,
        (None, true) => StartSession::Latest,
        _ => StartSession::New,
    };
    options.additional_dirs = cli
        .add_dirs
        .iter()
        .map(|dir| canonical_cwd(Some(dir)))
        .collect::<anyhow::Result<_>>()?;
    options.mcp_servers = config.mcp_servers(&launch.name)?;
    options.session_meta = config.session_meta(&launch.name);
    let (waker, wake_fd) = bus::wake_pair()?;
    let (events_tx, events_rx) = bus::event_channel(waker);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let mut acp_task = None;
    let agent_launch = launch.clone();
    let ui_result = ui::run(
        ui::UiOptions {
            agent: launch.name.clone(),
            cwd,
            launch,
        },
        events_rx,
        wake_fd,
        cmd_tx.clone(),
        || {
            acp_task = Some(runtime.spawn(acp::run_recorded(
                agent_launch,
                options,
                events_tx,
                cmd_rx,
                cli.record_acp,
            )));
        },
    );
    let _ = cmd_tx.send(UiCmd::Shutdown);
    if let Some(task) = acp_task {
        let finished =
            runtime.block_on(async { tokio::time::timeout(Duration::from_secs(2), task).await });
        if finished.is_err() {
            tracing::warn!("ACP task did not stop within two seconds");
        }
    }
    runtime.shutdown_timeout(Duration::from_secs(2));
    match ui_result? {
        ui::UiExit::Quit { resume } => {
            if let Some(id) = resume {
                eprintln!("Resume this session: {}", resume_command(&id));
            }
            Ok(())
        }
        ui::UiExit::NoTern => {
            eprintln!(
                "acp-tsp needs Tern (a terminal that speaks TSP). Run it in a Tern agent block; check TERN_TSP is not disabled."
            );
            std::process::exit(2)
        }
    }
}

/// The command line that reopens a session: this invocation's agent, directory and roots
/// with `--resume <id>`.
fn resume_command(cli: &Cli) -> impl Fn(&str) -> String + use<> {
    let mut words = vec!["acp-tsp".to_owned()];
    if let Some(agent) = &cli.agent {
        words.extend(["--agent".to_owned(), agent.clone()]);
    }
    if let Some(cwd) = &cli.cwd {
        words.extend(["--cwd".to_owned(), cwd.display().to_string()]);
    }
    for dir in &cli.add_dirs {
        words.extend(["--add-dir".to_owned(), dir.display().to_string()]);
    }
    let command = cli.command.clone();
    move |id: &str| {
        let mut words = words.clone();
        words.extend(["--resume".to_owned(), id.to_owned()]);
        if !command.is_empty() {
            words.push("--".to_owned());
            words.extend(command.iter().cloned());
        }
        words
            .iter()
            .map(|word| shell_quote(word))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// POSIX shell quoting, only where a word needs it.
fn shell_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
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
        tokio::time::timeout(Duration::from_secs(15), async move {
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
                    let request = InitializeRequest::new(ProtocolVersion::V1)
                        .client_info(Implementation::new("acp-tsp", env!("CARGO_PKG_VERSION")))
                        .client_capabilities(acp::capabilities());
                    let response = connection.send_request(request).block_task().await?;
                    println!("{}", serde_json::to_string_pretty(&response)?);
                    Ok(())
                })
                .await?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("agent probe timed out after 15 seconds")??;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_start_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["acp-tsp", "--resume", "one", "--continue"]).is_err());
        assert!(Cli::try_parse_from(["acp-tsp", "--resume", "--continue"]).is_err());
        assert_eq!(
            Cli::try_parse_from(["acp-tsp", "--resume", "s"])
                .unwrap()
                .resume,
            Some(Some("s".into()))
        );
        // Without an id (even before another flag) it opens the session picker.
        assert_eq!(
            Cli::try_parse_from(["acp-tsp", "--resume", "--agent", "codex"])
                .unwrap()
                .resume,
            Some(None)
        );
        assert!(
            Cli::try_parse_from(["acp-tsp", "--continue"])
                .unwrap()
                .continue_session
        );
    }
    #[test]
    fn resume_hint_repeats_the_agent_and_quotes_what_needs_it() {
        let cli = Cli::try_parse_from([
            "acp-tsp",
            "--agent",
            "codex",
            "--cwd",
            "/tmp/my project",
            "--continue",
        ])
        .unwrap();
        assert_eq!(
            resume_command(&cli)("019a-it's"),
            "acp-tsp --agent codex --cwd '/tmp/my project' --resume '019a-it'\\''s'"
        );
        let raw = Cli::try_parse_from(["acp-tsp", "--", "my-agent", "--acp"]).unwrap();
        assert_eq!(
            resume_command(&raw)("s1"),
            "acp-tsp --resume s1 -- my-agent --acp"
        );
    }

    #[test]
    fn agent_update_accepts_optional_agent_name() {
        assert!(Cli::try_parse_from(["acp-tsp", "agents", "update", "agy", "--write"]).is_ok());
        assert!(Cli::try_parse_from(["acp-tsp", "agents", "update", "--write"]).is_ok());
    }
}
