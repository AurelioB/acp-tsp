//! Registry-backed installation, authentication, and version checks for ACP agents.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const REGISTRY_URL: &str = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";
const TOS_NOTICE: &str = "Antigravity is proprietary. Google's FAQ says using third-party software to access Antigravity may violate its Terms of Service (https://antigravity.google/docs/faq).";

#[derive(Clone, Debug, Deserialize)]
pub struct Registry {
    pub agents: Vec<RegistryAgent>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct RegistryAgent {
    pub id: String,
    pub version: String,
    pub distribution: Distribution,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Distribution {
    pub binary: Option<BTreeMap<String, BinaryDistribution>>,
    pub npx: Option<PackageDistribution>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct BinaryDistribution {
    pub archive: String,
    pub sha256: Option<String>,
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct PackageDistribution {
    pub package: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallManifest {
    pub id: String,
    pub version: String,
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

pub fn command(
    action: &str,
    name: Option<&str>,
    version: Option<&str>,
    yes: bool,
    write_config: bool,
    auth_type: Option<&str>,
) -> anyhow::Result<()> {
    match action {
        "install" => {
            if name != Some("agy") {
                bail!("only `agents install agy` is supported");
            }
            if !yes {
                eprintln!("{TOS_NOTICE}\nInstall Antigravity ACP server? [y/N] ");
                if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    bail!("installation requires --yes when stdin is not a terminal");
                }
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    bail!("installation declined");
                }
            } else {
                eprintln!("{TOS_NOTICE}");
            }
            install_agy(version)
        }
        "auth" => {
            if name != Some("agy") {
                bail!("only `agents auth agy` is supported");
            }
            set_auth(auth_type.context(
                "choose --type oauth-personal|gemini-api-key|oauth-business|agent-platform",
            )?)
        }
        "update" => update(name, write_config),
        "list" => list(),
        _ => bail!("unknown agents action {action:?}"),
    }
}

pub fn registry_from_bytes(bytes: &[u8]) -> anyhow::Result<Registry> {
    Ok(serde_json::from_slice(bytes)?)
}
fn registry() -> anyhow::Result<Registry> {
    let bytes = reqwest::blocking::get(REGISTRY_URL)?
        .error_for_status()?
        .bytes()?;
    registry_from_bytes(&bytes)
}
fn find_entry<'a>(registry: &'a Registry, id: &str) -> anyhow::Result<&'a RegistryAgent> {
    registry
        .agents
        .iter()
        .find(|a| a.id == id)
        .with_context(|| format!("registry has no {id} entry"))
}

pub fn platform_key(os: &str, arch: &str) -> anyhow::Result<String> {
    let os = match os {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "windows",
        _ => bail!("unsupported platform OS {os}"),
    };
    let arch = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => bail!("unsupported platform architecture {arch}"),
    };
    Ok(format!("{os}-{arch}"))
}

fn data_dir() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .context("could not determine data directory")?;
    Ok(base.join("acp-tsp/agents/agy"))
}
fn install_agy(requested: Option<&str>) -> anyhow::Result<()> {
    let reg = registry()?;
    let entry = find_entry(&reg, "antigravity-acp")?;
    let version = requested.unwrap_or(&entry.version);
    if version != entry.version {
        bail!(
            "registry only provides Antigravity ACP {0}; requested {version}",
            entry.version
        );
    }
    if version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
    {
        bail!("unsafe registry version {version:?}");
    }
    let key = platform_key(std::env::consts::OS, std::env::consts::ARCH)?;
    let dist = entry
        .distribution
        .binary
        .as_ref()
        .context("antigravity-acp has no binary distribution")?
        .get(&key)
        .with_context(|| format!("antigravity-acp has no distribution for {key}"))?;
    safe_relative(dist.cmd.trim_start_matches("./"))?;
    let parent = data_dir()?;
    let target = parent.join(version);
    if target.exists() {
        bail!("installation already exists at {}", target.display());
    }
    std::fs::create_dir_all(&parent)?;
    let mut response = reqwest::blocking::get(&dist.archive)?.error_for_status()?;
    let total = response.content_length().unwrap_or(0);
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut last = 0;
    loop {
        let n = response.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..n]);
        if bytes.len() - last >= 8 * 1024 * 1024 {
            eprint!(
                "\rDownloading Antigravity ACP: {} MiB{}",
                bytes.len() / 1024 / 1024,
                if total > 0 {
                    format!(" / {} MiB", total / 1024 / 1024)
                } else {
                    String::new()
                }
            );
            last = bytes.len();
        }
    }
    eprintln!(
        "\rDownloaded Antigravity ACP archive ({} MiB).",
        bytes.len() / 1024 / 1024
    );
    verify_sha256(&bytes, dist.sha256.as_deref())?;
    let staging = parent.join(format!(".{version}.tmp-{}", std::process::id()));
    std::fs::create_dir(&staging)?;
    let extraction = extract_archive(&bytes, &dist.archive, &dist.cmd, &staging);
    if let Err(error) = extraction {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    let manifest = InstallManifest {
        id: entry.id.clone(),
        version: version.to_owned(),
        cmd: dist.cmd.clone(),
        args: dist.args.clone(),
        env: dist.env.clone(),
    };
    std::fs::write(
        staging.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::rename(&staging, &target)?;
    set_current(&parent, version)?;
    println!(
        "Installed agy {version}: {} {}",
        dist.cmd,
        dist.args.join(" ")
    );
    Ok(())
}

pub fn verify_sha256(bytes: &[u8], expected: Option<&str>) -> anyhow::Result<()> {
    if let Some(expected) = expected {
        let actual = format!("{:x}", Sha256::digest(bytes));
        if !actual.eq_ignore_ascii_case(expected) {
            bail!("archive sha256 mismatch (expected {expected}, got {actual})");
        }
    }
    Ok(())
}
fn safe_relative(path: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("archive contains unsafe path {}", path.display());
    }
    Ok(path.to_path_buf())
}
pub fn extract_zip(bytes: &[u8], destination: &Path) -> anyhow::Result<()> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let rel = safe_relative(file.name())?;
        let out = destination.join(rel);
        if file.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        std::fs::create_dir_all(out.parent().context("archive entry has no parent")?)?;
        let mut output = std::fs::File::create(out)?;
        std::io::copy(&mut file, &mut output)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                std::fs::set_permissions(
                    destination.join(safe_relative(file.name())?),
                    std::fs::Permissions::from_mode(mode & 0o777),
                )?;
            }
        }
    }
    Ok(())
}
fn extract_archive(
    bytes: &[u8],
    url: &str,
    command: &str,
    destination: &Path,
) -> anyhow::Result<()> {
    let lower = url.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        return extract_zip(bytes, destination);
    }
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        return extract_tar(flate2::read::GzDecoder::new(bytes), destination);
    }
    if lower.ends_with(".tar.bz2") || lower.ends_with(".tbz2") {
        return extract_tar(bzip2::read::BzDecoder::new(bytes), destination);
    }
    let relative = safe_relative(command.trim_start_matches("./"))?;
    let output = destination.join(relative);
    std::fs::create_dir_all(output.parent().context("binary command has no parent")?)?;
    std::fs::write(&output, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}
fn extract_tar<R: Read>(reader: R, destination: &Path) -> anyhow::Result<()> {
    let mut archive = tar::Archive::new(reader);
    for item in archive.entries()? {
        let mut entry = item?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let rel = safe_relative(&path)?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            bail!(
                "archive contains unsupported link or special entry {}",
                rel.display()
            );
        }
        let out = destination.join(rel);
        if kind.is_dir() {
            std::fs::create_dir_all(out)?;
        } else {
            std::fs::create_dir_all(out.parent().context("archive entry has no parent")?)?;
            entry.unpack(out)?;
        }
    }
    Ok(())
}
fn set_current(parent: &Path, version: &str) -> anyhow::Result<()> {
    let current = parent.join("current");
    let temp = parent.join(format!(".current-{}", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    #[cfg(unix)]
    std::os::unix::fs::symlink(version, &temp)?;
    #[cfg(windows)]
    std::fs::write(&temp, version)?;
    if current.exists() || current.symlink_metadata().is_ok() {
        std::fs::remove_file(&current)?;
    }
    std::fs::rename(temp, current)?;
    Ok(())
}

pub fn installed_manifest() -> anyhow::Result<InstallManifest> {
    installed_manifest_from(&data_dir()?)
}
fn installed_manifest_from(base: &Path) -> anyhow::Result<InstallManifest> {
    #[cfg(windows)]
    let version = std::fs::read_to_string(base.join("current"))?;
    #[cfg(not(windows))]
    let version = std::fs::read_link(base.join("current"))?
        .to_string_lossy()
        .into_owned();
    let path = base.join(version.trim()).join("manifest.json");
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
pub fn resolve_agy() -> anyhow::Result<crate::launch::AgentLaunch> {
    resolve_agy_from(&data_dir()?)
}
fn resolve_agy_from(base: &Path) -> anyhow::Result<crate::launch::AgentLaunch> {
    let manifest = installed_manifest_from(base)
        .context("agy is not installed; run `acp-tsp agents install agy`")?;
    #[cfg(windows)]
    let executable = base.join(&manifest.version).join(&manifest.cmd);
    #[cfg(not(windows))]
    let executable = base.join("current").join(&manifest.cmd);
    Ok(crate::launch::AgentLaunch {
        name: "agy".into(),
        command: executable.to_string_lossy().into_owned(),
        args: manifest.args,
        env: manifest.env,
    })
}

fn set_auth(auth_type: &str) -> anyhow::Result<()> {
    let path = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("could not determine home directory")?
        .join(".gemini/antigravity-acp/settings.json");
    set_auth_at(&path, auth_type)?;
    println!("Configured agy auth type {auth_type}.");
    match auth_type {
        "oauth-personal" => println!("The first session requires a one-time browser login."),
        "gemini-api-key" => {
            println!("Set GEMINI_API_KEY in the environment before starting a session.")
        }
        _ => println!("Start a session to complete the selected authentication flow."),
    }
    Ok(())
}
fn set_auth_at(path: &Path, auth_type: &str) -> anyhow::Result<()> {
    const VALID: &[&str] = &[
        "oauth-personal",
        "gemini-api-key",
        "oauth-business",
        "agent-platform",
    ];
    if !VALID.contains(&auth_type) {
        bail!("unsupported agy auth type {auth_type:?}");
    }
    let mut value: Value = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("settings file is not valid JSON")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e.into()),
    };
    if !value.is_object() {
        bail!("{} must contain a JSON object", path.display());
    }
    if !value.get("auth").is_some_and(Value::is_object) {
        value["auth"] = serde_json::json!({});
    }
    value["auth"]["type"] = Value::String(auth_type.to_owned());
    atomic_json(path, &value)?;
    Ok(())
}
fn atomic_json(path: &Path, value: &Value) -> anyhow::Result<()> {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let bytes = serde_json::to_vec_pretty(value)?;
    let parent = resolved.parent().context("settings file has no parent")?;
    std::fs::create_dir_all(parent)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = parent.join(format!(".settings-{}-{nonce}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if resolved.exists() {
        let backup = unique_backup_path(&resolved)?;
        let mut original = std::fs::File::open(&resolved)?;
        let mut backup_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)?;
        if let Err(error) = std::io::copy(&mut original, &mut backup_file) {
            let _ = std::fs::remove_file(&backup);
            return Err(error.into());
        }
        backup_file.sync_all()?;
    }
    if let Err(error) = std::fs::rename(&temp, &resolved) {
        let _ = std::fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}
fn unique_backup_path(path: &Path) -> anyhow::Result<PathBuf> {
    let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    for suffix in 0u64.. {
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

fn list() -> anyhow::Result<()> {
    match installed_manifest() {
        Ok(m) => println!(
            "agy: installed {} — {} {}",
            m.version,
            m.cmd,
            m.args.join(" ")
        ),
        Err(_) => println!("agy: not installed"),
    }
    Ok(())
}
fn update(name: Option<&str>, write_config: bool) -> anyhow::Result<()> {
    const PRESETS: &[(&str, &str)] = &[
        ("claude-acp", "claude"),
        ("codex-acp", "codex"),
        ("antigravity-acp", "agy"),
    ];
    if let Some(name) = name
        && !PRESETS
            .iter()
            .any(|(id, preset)| name == *id || name == *preset)
    {
        bail!("unknown agent {name:?}; supported updates: claude, codex, agy");
    }
    let reg = registry()?;
    let config = crate::config::Config::load()?;
    let agy_installed = installed_manifest()
        .ok()
        .map(|manifest| manifest.version)
        .unwrap_or_else(|| "not installed".into());
    let pinned = [
        ("claude-acp", "claude", "0.87.0".to_owned()),
        ("codex-acp", "codex", "2.1.1".to_owned()),
        ("antigravity-acp", "agy", agy_installed),
    ];
    let mut overrides = Vec::new();
    for (id, preset, pin) in pinned {
        if name.is_some_and(|name| name != id && name != preset) {
            continue;
        }
        let entry = find_entry(&reg, id)?;
        println!(
            "{preset}: registry {} (preset {pin}){}",
            entry.version,
            if pin == entry.version {
                ""
            } else {
                " — update available"
            }
        );
        if write_config && pin != entry.version {
            if preset == "agy" {
                continue;
            }
            let package = entry
                .distribution
                .npx
                .as_ref()
                .context("registry entry has no npx distribution")?
                .package
                .clone();
            overrides.push((preset, package));
        }
    }
    if write_config {
        write_overrides(&overrides)?;
    }
    // Keep known_agents/config access live for callers; this ensures path matches regular user config.
    let _ = config.known_agents();
    Ok(())
}
fn write_overrides(overrides: &[(&str, String)]) -> anyhow::Result<()> {
    if overrides.is_empty() {
        return Ok(());
    }
    let config_path = std::env::var_os("ACP_TSP_CONFIG")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(|p| PathBuf::from(p).join("acp-tsp/config.toml"))
        })
        .or_else(|| {
            std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config/acp-tsp/config.toml"))
        })
        .context("could not determine user config path")?;
    let mut value: toml::Value = match std::fs::read_to_string(&config_path) {
        Ok(s) => toml::from_str(&s)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(Default::default())
        }
        Err(e) => return Err(e.into()),
    };
    for (name, package) in overrides {
        let agents = value
            .as_table_mut()
            .context("config root must be a table")?
            .entry("agents")
            .or_insert_with(|| toml::Value::Table(Default::default()));
        let agent = agents
            .as_table_mut()
            .context("agents config must be a table")?
            .entry(*name)
            .or_insert_with(|| toml::Value::Table(Default::default()));
        let table = agent
            .as_table_mut()
            .context("agent config must be a table")?;
        table.insert("command".into(), toml::Value::String("npx".into()));
        table.insert(
            "args".into(),
            toml::Value::Array(vec![
                toml::Value::String("-y".into()),
                toml::Value::String(package.clone()),
            ]),
        );
    }
    std::fs::create_dir_all(config_path.parent().context("config path has no parent")?)?;
    std::fs::write(config_path, toml::to_string_pretty(&value)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_all_platform_keys() {
        assert_eq!(platform_key("linux", "x86_64").unwrap(), "linux-x86_64");
        assert_eq!(platform_key("linux", "aarch64").unwrap(), "linux-aarch64");
        assert_eq!(platform_key("macos", "aarch64").unwrap(), "darwin-aarch64");
        assert_eq!(platform_key("macos", "x86_64").unwrap(), "darwin-x86_64");
        assert_eq!(
            platform_key("windows", "aarch64").unwrap(),
            "windows-aarch64"
        );
        assert_eq!(platform_key("windows", "x86_64").unwrap(), "windows-x86_64");
    }
    #[test]
    fn rejects_sha256_mismatch_for_archive_file() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive.zip");
        std::fs::write(&archive, b"archive bytes").unwrap();
        let bytes = std::fs::read(&archive).unwrap();
        assert!(verify_sha256(&bytes, Some(&"0".repeat(64))).is_err());
    }
    #[test]
    fn rejects_zip_path_traversal() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut bytes);
            zip.start_file("../escape", zip::write::FileOptions::<()>::default())
                .unwrap();
            zip.write_all(b"bad").unwrap();
            zip.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(extract_zip(bytes.get_ref(), dir.path()).is_err());
        assert!(!dir.path().parent().unwrap().join("escape").exists());
    }
    #[test]
    fn registry_fixture_deserializes_binary_distribution() {
        let registry =
            registry_from_bytes(include_bytes!("../tests/fixtures/agy-registry.json")).unwrap();
        let entry = find_entry(&registry, "antigravity-acp").unwrap();
        assert_eq!(entry.version, "1.3.0");
        assert_eq!(
            entry.distribution.binary.as_ref().unwrap()["linux-x86_64"].args,
            ["--uid="]
        );
    }
    #[cfg(unix)]
    #[test]
    fn auth_update_preserves_settings_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("real-settings.json");
        let link = dir.path().join("settings.json");
        std::fs::write(&actual, r#"{"kept":true}"#).unwrap();
        std::os::unix::fs::symlink(&actual, &link).unwrap();
        set_auth_at(&link, "oauth-personal").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(&actual).unwrap()).unwrap();
        assert_eq!(saved["kept"], true);
        assert_eq!(saved["auth"]["type"], "oauth-personal");
    }
    #[test]
    fn manifest_resolves_preset_command_and_args() {
        let dir = tempfile::tempdir().unwrap();
        let version = dir.path().join("1.3.0");
        std::fs::create_dir(&version).unwrap();
        let manifest = InstallManifest {
            id: "antigravity-acp".into(),
            version: "1.3.0".into(),
            cmd: "./agy-server".into(),
            args: vec!["--uid=".into()],
            env: BTreeMap::from([("EXAMPLE".into(), "value".into())]),
        };
        std::fs::write(
            version.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("1.3.0", dir.path().join("current")).unwrap();
        #[cfg(windows)]
        std::fs::write(dir.path().join("current"), "1.3.0").unwrap();
        let launch = resolve_agy_from(dir.path()).unwrap();
        assert!(launch.command.ends_with("agy-server"));
        assert_eq!(launch.args, ["--uid="]);
        assert_eq!(launch.env["EXAMPLE"], "value");
    }
    #[test]
    fn auth_update_merges_and_backs_up_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"theme":"dark","auth":{"other":true}}"#).unwrap();
        set_auth_at(&path, "gemini-api-key").unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["theme"], "dark");
        assert_eq!(saved["auth"]["other"], true);
        assert_eq!(saved["auth"]["type"], "gemini-api-key");
        assert!(std::fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.bak-")
        }));
    }
}
