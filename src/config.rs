//! User configuration and built-in agent presets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::launch::AgentLaunch;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub default_agent: Option<String>,
    pub agents: BTreeMap<String, AgentOverride>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AgentOverride {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: BTreeMap<String, String>,
}

impl Config {
    /// Loads configuration from ACP_TSP_CONFIG or the platform XDG config path.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(config_path()?)
    }

    fn load_from(path: PathBuf) -> anyhow::Result<Self> {
        match std::fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents)
                .with_context(|| format!("could not parse config file {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("could not read {}", path.display())),
        }
    }

    /// Resolves a named preset or configured agent, using `default_agent` when omitted.
    pub fn resolve(&self, name: Option<&str>) -> anyhow::Result<AgentLaunch> {
        let selected = name.or(self.default_agent.as_deref()).unwrap_or("claude");
        let mut launch = match selected {
            "agy" | "claude" | "codex" => preset(selected)?,
            _ => match self.agents.get(selected) {
                Some(overrides) => AgentLaunch {
                    name: selected.to_owned(),
                    command: overrides.command.clone().context(format!(
                        "configured agent {selected:?} needs a command in [agents.{selected}]"
                    ))?,
                    args: overrides.args.clone().unwrap_or_default(),
                    env: BTreeMap::new(),
                },
                None => bail!(
                    "unknown agent {selected:?}; known agents: {}",
                    self.known_agents().join(", ")
                ),
            },
        };
        if let Some(overrides) = self.agents.get(selected) {
            if let Some(command) = &overrides.command {
                launch.command.clone_from(command);
            }
            if let Some(args) = &overrides.args {
                launch.args.clone_from(args);
            }
            launch.env.extend(overrides.env.clone());
        }
        if selected == "agy" && !Path::new(&launch.command).is_file() {
            bail!(
                "agy is not installed: expected {} (installation is available in a later release)",
                launch.command
            );
        }
        Ok(launch)
    }

    pub fn known_agents(&self) -> Vec<String> {
        let mut names = ["agy", "claude", "codex"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        for name in self.agents.keys() {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names.sort();
        names
    }
}

fn preset(name: &str) -> anyhow::Result<AgentLaunch> {
    let empty = BTreeMap::new();
    let (command, args) = match name {
        "claude" => (
            "npx".to_owned(),
            vec![
                "-y".into(),
                "@agentclientprotocol/claude-agent-acp@0.87.0".into(),
            ],
        ),
        "codex" => (
            "npx".to_owned(),
            vec!["-y".into(), "@agentclientprotocol/codex-acp@2.1.1".into()],
        ),
        "agy" => {
            let mut path = data_home()?;
            path.push("acp-tsp/agents/agy/agy_acp_server");
            if cfg!(windows) {
                path.set_extension("exe");
            } else {
                path.set_extension("par");
            }
            let args = if cfg!(target_os = "linux") {
                vec!["--uid=".into()]
            } else {
                Vec::new()
            };
            (path.to_string_lossy().into_owned(), args)
        }
        _ => bail!(
            "unknown agent {name:?}; known agents: agy, claude, codex (or add it under [agents.{name}])"
        ),
    };
    Ok(AgentLaunch {
        name: name.into(),
        command,
        args,
        env: empty,
    })
}

fn config_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("ACP_TSP_CONFIG") {
        return Ok(path.into());
    }
    let mut path = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(home_config_dir)?;
    path.push("acp-tsp/config.toml");
    Ok(path)
}

fn home_config_dir() -> anyhow::Result<PathBuf> {
    Ok(home_dir()?.join(".config"))
}

fn data_home() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(path.into());
    }
    Ok(home_dir()?.join(".local/share"))
}

fn home_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .context("could not determine home directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_overrides_preset_and_merges_environment() {
        let config: Config = toml::from_str(
            r#"
            [agents.claude]
            command = "custom-npx"
            args = ["--custom"]
            env = { EXTRA = "yes", KEEP = "override" }
        "#,
        )
        .unwrap();
        let launch = config.resolve(Some("claude")).unwrap();
        assert_eq!(launch.command, "custom-npx");
        assert_eq!(launch.args, ["--custom"]);
        assert_eq!(launch.env.get("EXTRA").map(String::as_str), Some("yes"));
    }

    #[test]
    fn default_agent_is_used() {
        let config: Config = toml::from_str("default_agent = 'codex'").unwrap();
        assert_eq!(
            config.resolve(None).unwrap().args[1],
            "@agentclientprotocol/codex-acp@2.1.1"
        );
    }

    #[test]
    fn unknown_agent_names_known_agents() {
        let error = Config::default()
            .resolve(Some("missing"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("agy, claude, codex"));
    }

    #[test]
    fn configured_custom_agent_is_resolved() {
        let config: Config =
            toml::from_str("[agents.custom]\ncommand='agent'\nargs=['--stdio']").unwrap();
        let launch = config.resolve(Some("custom")).unwrap();
        assert_eq!(launch.command, "agent");
        assert_eq!(launch.args, ["--stdio"]);
    }
}
