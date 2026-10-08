//! MCP-over-ACP `tern_open` tool, attached to newly created sessions.
use std::path::{Path, PathBuf};

use agent_client_protocol::mcp_server::{McpOutcome, McpRequest, McpRequestContext, McpService};
use agent_client_protocol::{Agent, Error};
use futures::future::BoxFuture;
use serde_json::{Value, json};

pub(super) struct TernTools {
    roots: Vec<PathBuf>,
}

impl TernTools {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    fn allowed(&self, path: &Path) -> bool {
        path.is_absolute()
            && std::fs::canonicalize(path)
                .is_ok_and(|canonical| self.roots.iter().any(|root| canonical.starts_with(root)))
    }
}

impl McpService<Agent> for TernTools {
    fn execute(
        &self,
        request: McpRequest,
        _context: McpRequestContext<Agent>,
    ) -> BoxFuture<'static, Result<McpOutcome, Error>> {
        match request.method.as_str() {
            "tools/list" => Box::pin(async {
                Ok(McpOutcome::Result(json!({"tools": [{
                    "name": "tern_open", "description": "Open a file in Tern",
                    "inputSchema": {"type": "object", "properties": {
                        "path": {"type":"string"}, "line": {"type":"integer", "minimum":1}
                    }, "required":["path"]}
                }]})))
            }),
            "tools/call" => {
                let args = request.params.as_ref().and_then(|p| p.get("arguments"));
                let name = request
                    .params
                    .as_ref()
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str);
                let path = args
                    .and_then(|a| a.get("path"))
                    .and_then(Value::as_str)
                    .map(PathBuf::from);
                let line = args.and_then(|a| a.get("line")).and_then(Value::as_u64);
                let invalid_line =
                    args.and_then(|a| a.get("line")).is_some() && line.is_none_or(|line| line == 0);
                let allowed = name == Some("tern_open")
                    && path.as_ref().is_some_and(|p| self.allowed(p))
                    && !invalid_line;
                Box::pin(async move {
                    if !allowed {
                        return Ok(tool_error(
                            "Invalid tool, path outside session roots, or invalid line",
                        ));
                    }
                    let path = path.expect("validated path");
                    let target = match line {
                        Some(line) => format!("{}:{line}", path.display()),
                        None => path.display().to_string(),
                    };
                    let result = tokio::process::Command::new("tern")
                        .arg("open")
                        .arg(target)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .await;
                    Ok(match result {
                        Ok(status) if status.success() => McpOutcome::Result(
                            json!({"content":[{"type":"text","text":"Opened in Tern"}]}),
                        ),
                        _ => tool_error("Could not run tern open (Tern CLI unavailable or failed)"),
                    })
                })
            }
            _ => Box::pin(async { Err(Error::method_not_found()) }),
        }
    }
}

fn tool_error(message: &str) -> McpOutcome {
    McpOutcome::Result(json!({"content":[{"type":"text","text":message}], "isError":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_roots_reject_symlinks_outside() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("ok"), "").unwrap();
        std::fs::write(outside.path().join("secret"), "").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("link"))
            .unwrap();
        let tools = TernTools::new(vec![root.path().canonicalize().unwrap()]);
        assert!(tools.allowed(&root.path().join("ok")));
        assert!(!tools.allowed(&root.path().join("link")));
        assert!(!tools.allowed(&outside.path().join("secret")));
    }
}
