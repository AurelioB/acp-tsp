//! Safe editing of Tern's user settings.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};

pub fn settings_path(override_path: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(path) = override_path {
        return Ok(path.to_owned());
    }
    if let Some(path) = std::env::var_os("TERN_CONFIG_DIR") {
        return Ok(PathBuf::from(path).join("settings.json"));
    }
    #[cfg(target_os = "macos")]
    let path = home_dir()?.join("Library/Application Support/Tern/settings.json");
    #[cfg(target_os = "linux")]
    let path = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(home_dir()?.join(".config"))
        .join("tern/settings.json");
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let path = home_dir()?.join(".config/tern/settings.json");
    Ok(path)
}

pub fn agent_command(executable: &Path, agent: Option<&str>) -> String {
    let exe = shell_quote(&executable.to_string_lossy());
    match agent {
        Some(name) => format!("{exe} --agent {}", shell_quote(name)),
        None => exe,
    }
}

pub fn print_snippet(
    executable: &Path,
    agent: Option<&str>,
    new_blocks_agent: bool,
) -> anyhow::Result<()> {
    let mut snippet = Map::new();
    snippet.insert(
        "agent_command".into(),
        json!(agent_command(executable, agent)),
    );
    if new_blocks_agent {
        snippet.insert("new_blocks".into(), json!("Agent"));
    }
    println!("{}", serde_json::to_string_pretty(&Value::Object(snippet))?);
    Ok(())
}

pub fn update_settings(
    path: &Path,
    executable: &Path,
    agent: Option<&str>,
    new_blocks_agent: bool,
) -> anyhow::Result<Vec<String>> {
    let mut settings = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).with_context(|| {
            format!(
                "{} is not valid JSON; refusing to overwrite it",
                path.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    if !settings.is_object() {
        bail!(
            "{} must contain a JSON object; refusing to overwrite it",
            path.display()
        );
    }
    let mut changes = Vec::new();
    let object = settings.as_object_mut().expect("object checked");
    let command = agent_command(executable, agent);
    if object.get("agent_command") != Some(&json!(command)) {
        object.insert("agent_command".into(), json!(command));
        changes.push("agent_command".to_owned());
    }
    if new_blocks_agent && object.get("new_blocks") != Some(&json!("Agent")) {
        object.insert("new_blocks".into(), json!("Agent"));
        changes.push("new_blocks".to_owned());
    }
    if changes.is_empty() {
        return Ok(changes);
    }
    let bytes = serde_json::to_vec_pretty(&settings)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("could not create {}", parent.display()))?;
    let temporary = unique_path(parent, ".settings.json.tmp")?;
    let write_result = (|| -> anyhow::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        if path.exists() {
            let backup = unique_backup_path(path)?;
            let mut original = std::fs::File::open(path)?;
            let mut backup_file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&backup)
                .with_context(|| format!("could not create backup {}", backup.display()))?;
            if let Err(error) = std::io::copy(&mut original, &mut backup_file) {
                let _ = std::fs::remove_file(&backup);
                return Err(error).context("could not copy settings backup");
            }
            backup_file.sync_all()?;
        }
        std::fs::rename(&temporary, path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result?;
    Ok(changes)
}

fn unique_backup_path(path: &Path) -> anyhow::Result<PathBuf> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    for suffix in 0.. {
        let name = if suffix == 0 {
            format!("settings.json.bak-{unix}")
        } else {
            format!("settings.json.bak-{unix}-{suffix}")
        };
        let candidate = path.with_file_name(name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    unreachable!()
}

fn unique_path(dir: &Path, prefix: &str) -> anyhow::Result<PathBuf> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    for suffix in 0.. {
        let candidate = dir.join(format!("{prefix}-{}-{nonce}-{suffix}", std::process::id()));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    unreachable!()
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./-".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn home_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("could not determine home directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_other_keys_and_makes_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"theme":"dark","nested":{"keep":true}}"#).unwrap();
        let changed =
            update_settings(&path, Path::new("/opt/acp-tsp"), Some("claude"), true).unwrap();
        assert_eq!(changed, ["agent_command", "new_blocks"]);
        let result: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(result["theme"], "dark");
        assert_eq!(result["nested"]["keep"], true);
        assert_eq!(result["new_blocks"], "Agent");
        assert!(std::fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.bak-")
        }));
    }

    #[test]
    fn unchanged_settings_are_noop_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let command = agent_command(Path::new("/opt/acp-tsp"), Some("claude"));
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({"agent_command":command})).unwrap(),
        )
        .unwrap();
        assert!(
            update_settings(&path, Path::new("/opt/acp-tsp"), Some("claude"), false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn temporary_write_failure_leaves_settings_unchanged() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = br#"{"theme":"dark"}"#;
        std::fs::write(&path, original).unwrap();
        let permissions = std::fs::metadata(dir.path()).unwrap().permissions();
        let mut readonly = permissions.clone();
        readonly.set_mode(0o555);
        std::fs::set_permissions(dir.path(), readonly).unwrap();

        let probe = dir.path().join("write-probe");
        if std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .is_ok()
        {
            std::fs::remove_file(probe).unwrap();
            std::fs::set_permissions(dir.path(), permissions).unwrap();
            return;
        }

        let result = update_settings(&path, Path::new("/opt/acp-tsp"), Some("claude"), false);
        let current = std::fs::read(&path).unwrap();
        std::fs::set_permissions(dir.path(), permissions).unwrap();
        assert!(result.is_err());
        assert_eq!(current, original);
    }

    #[test]
    fn invalid_json_is_refused_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(update_settings(&path, Path::new("/bin/acp-tsp"), None, false).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "not json");
    }

    #[test]
    fn command_quotes_shell_metacharacters_and_existing_backups_are_preserved() {
        assert_eq!(
            agent_command(Path::new("/path with spaces/acp-tsp;"), None),
            "'/path with spaces/acp-tsp;'"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = br#"{"theme":"dark"}"#;
        std::fs::write(&path, original).unwrap();
        let existing_backup = unique_backup_path(&path).unwrap();
        std::fs::write(&existing_backup, "keep existing backup").unwrap();
        update_settings(&path, Path::new("/opt/acp-tsp"), Some("claude"), false).unwrap();
        assert_eq!(
            std::fs::read(&existing_backup).unwrap(),
            b"keep existing backup"
        );
        let existing_backup_name = existing_backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(std::fs::read_dir(dir.path()).unwrap().any(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            name.starts_with("settings.json.bak-") && name != existing_backup_name
        }));
    }
}
