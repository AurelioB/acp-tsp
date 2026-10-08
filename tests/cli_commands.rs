use std::process::Command;

use serde_json::Value;

#[test]
fn help_lists_stage_one_commands_and_launch_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_acp-tsp"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for expected in ["--agent", "--cwd", "agents", "setup-tern", "probe"] {
        assert!(
            help.contains(expected),
            "missing {expected:?} in help: {help}"
        );
    }
}

#[test]
fn setup_tern_prints_json_without_writing_settings() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings.json");
    let output = Command::new(env!("CARGO_BIN_EXE_acp-tsp"))
        .args([
            "--agent",
            "codex",
            "setup-tern",
            "--settings",
            settings.to_str().unwrap(),
            "--print",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let snippet: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        snippet["agent_command"]
            .as_str()
            .unwrap()
            .ends_with(" --agent codex")
    );
    assert!(snippet.get("new_blocks").is_none());
    assert!(!settings.exists());
}
