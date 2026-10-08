//! Agent-specific deviations, decided once for the ACP side and the UI.

use agent_client_protocol::schema::v1::InitializeResponse;

/// The Codex adapter's `agentInfo.name`.
pub const CODEX_ACP: &str = "@agentclientprotocol/codex-acp";

/// Whether to steer a running turn with `_session/steering` instead of queueing the next
/// prompt.
///
/// Requires the agent's `_meta.steering.supported`. Codex 2.1.1 advertises it but ignores
/// `idleBehavior: "promptRequired"`: steering that races the end of a turn starts a detached
/// turn whose `session/prompt` response the client never sees. Queueing is the safe choice
/// there.
pub fn steering(init: &InitializeResponse) -> bool {
    let advertised = init
        .meta
        .as_ref()
        .and_then(|meta| meta.get("steering"))
        .and_then(|steering| steering.get("supported"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let codex = init
        .agent_info
        .as_ref()
        .is_some_and(|info| info.name == CODEX_ACP);
    advertised && !codex
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::ProtocolVersion;
    use agent_client_protocol::schema::v1::Implementation;

    use super::*;

    fn init(name: &str, supported: bool) -> InitializeResponse {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "steering".into(),
            serde_json::json!({ "supported": supported }),
        );
        InitializeResponse::new(ProtocolVersion::V1)
            .agent_info(Implementation::new(name, "1"))
            .meta(meta)
    }

    #[test]
    fn steering_needs_advertisement_and_reliable_idle_semantics() {
        assert!(steering(&init(
            "@agentclientprotocol/claude-agent-acp",
            true
        )));
        assert!(!steering(&init(
            "@agentclientprotocol/claude-agent-acp",
            false
        )));
        assert!(!steering(&init(CODEX_ACP, true)));
    }
}
