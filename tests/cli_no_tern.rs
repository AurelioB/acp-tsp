use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn absent_tern_never_starts_agent_process() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("agent-started");
    let npx = temp.path().join("npx");
    std::fs::write(
        &npx,
        format!("#!/bin/sh\nprintf started > '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&npx, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Give the real UI a tty; without one WakeTerm::open fails before TSP detection.
    let output = Command::new("/usr/bin/script")
        .args([
            "-e",
            "-q",
            "-c",
            &format!("'{}' --agent claude", env!("CARGO_BIN_EXE_acp-tsp")),
            "/dev/null",
        ])
        .env("TERN_TSP", "0")
        .env("PATH", temp.path())
        .env("ACP_TSP_CONFIG", temp.path().join("missing.toml"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stdout).contains("acp-tsp needs Tern"));
    assert!(
        !marker.exists(),
        "agent executable ran before Tern detection"
    );
}
