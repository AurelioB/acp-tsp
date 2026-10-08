//! Read Codex account limits via the adapter's bundled CLI app-server, not the ACP session.
use std::{process::Stdio, time::Duration};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdout, Command};

use crate::{
    launch::AgentLaunch,
    model::usage::{CodexUsage, parse_codex},
};

async fn response(lines: &mut Lines<BufReader<ChildStdout>>, id: u64) -> Result<Value, String> {
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|_| "Codex sidecar read failed")?
    {
        let message: Value =
            serde_json::from_str(&line).map_err(|_| "Invalid Codex sidecar response")?;
        // Notifications and responses to other requests cannot satisfy this request.
        if message.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        // The bundled app-server omits `jsonrpc` on responses; validate it if supplied.
        if message.get("jsonrpc").is_some()
            && message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        {
            return Err("Invalid Codex sidecar protocol".into());
        }
        if message.get("error").is_some() {
            // Error messages may contain private account data: never forward the payload.
            return Err("Codex rate-limits request failed".into());
        }
        return message
            .get("result")
            .cloned()
            .ok_or_else(|| "Missing Codex sidecar result".into());
    }
    Err("Codex sidecar closed without a response".into())
}

async fn send(stdin: &mut tokio::process::ChildStdin, value: &Value) -> Result<(), String> {
    stdin
        .write_all(value.to_string().as_bytes())
        .await
        .map_err(|_| "Codex sidecar write failed".to_owned())?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|_| "Codex sidecar write failed".to_owned())
}

async fn fetch_inner(launch: &AgentLaunch) -> Result<CodexUsage, String> {
    let mut child = Command::new(&launch.command)
        .args(&launch.args)
        .args(["cli", "app-server"])
        .envs(&launch.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Could not start Codex sidecar".to_owned())?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("Codex sidecar stdin unavailable")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Codex sidecar stdout unavailable")?;
    let mut lines = BufReader::new(stdout).lines();
    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"clientInfo":{"name":"acp-tsp","version":env!("CARGO_PKG_VERSION")}}}),
    )
    .await?;
    // This sidecar sends only initialize, initialized and account/rateLimits/read:
    // never request consume/redeem (or mutate the account).
    response(&mut lines, 1).await?;
    send(&mut stdin, &json!({"jsonrpc":"2.0","method":"initialized"})).await?;
    send(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":2,"method":"account/rateLimits/read"}),
    )
    .await?;
    let result = response(&mut lines, 2).await?;
    parse_codex(result)
}

/// One short-lived process per read. Timed-out and dropped requests kill their child.
pub async fn fetch(launch: &AgentLaunch) -> Result<CodexUsage, String> {
    tokio::time::timeout(Duration::from_secs(20), fetch_inner(launch))
        .await
        .map_err(|_| "Codex rate-limits request timed out".to_owned())?
}
