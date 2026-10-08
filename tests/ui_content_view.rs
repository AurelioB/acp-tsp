use acp_tsp::{
    event::{BridgeEvent, TerminalEvent},
    model::Model,
    ui::{composer::Composer, content, view::view},
};
use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, CreateElicitationRequest, ElicitationFormMode, ElicitationSchema,
    ElicitationSessionScope, EmbeddedResource, EmbeddedResourceResource, ImageContent,
    IntegerPropertySchema, ResourceLink, SessionUpdate, StringPropertySchema, Terminal,
    TerminalExitStatus, TextResourceContents, ToolCall, ToolCallContent,
};
use serde_json::Value;
use tern_sdk::{Capabilities, reconcile::Doc};

fn wire(model: &Model) -> Value {
    let doc = Doc::from_view(view(model, &Composer::default(), &Capabilities::default())).unwrap();
    let mut regions = serde_json::Map::new();
    for (name, tree) in ["main", "dock", "layer"].into_iter().zip(doc.regions()) {
        if let Some(tree) = tree {
            regions.insert(name.into(), serde_json::to_value(tree.wire()).unwrap());
        }
    }
    Value::Object(regions)
}

#[test]
fn image_and_embedded_resource_snapshot() {
    let mut model = Model::default();
    let image = ImageContent::new("aW1hZ2U=", "image/png");
    model
        .content
        .blobs
        .insert(content::image_key(&image.data), "sha256-image".into());
    for block in [
        ContentBlock::Image(image),
        ContentBlock::Resource(EmbeddedResource::new(
            EmbeddedResourceResource::TextResourceContents(
                TextResourceContents::new("fn main() {}", "file:///src/main.rs")
                    .mime_type("text/x-rust"),
            ),
        )),
        ContentBlock::ResourceLink(ResourceLink::new("source", "file:///src/main.rs")),
    ] {
        model.apply(BridgeEvent::Update(Box::new(
            SessionUpdate::AgentMessageChunk(ContentChunk::new(block)),
        )));
    }
    insta::assert_json_snapshot!("ui_rich_content", wire(&model)["main"]);
}

#[test]
fn terminal_tool_live_card_snapshot() {
    let mut model = Model::default();
    let id: agent_client_protocol::schema::v1::TerminalId = "term".into();
    model.apply(BridgeEvent::Terminal(TerminalEvent::Created {
        id: id.clone(),
        command: "cargo test".into(),
        cwd: "/tmp".into(),
    }));
    model.apply(BridgeEvent::Terminal(TerminalEvent::Output {
        id: id.clone(),
        chunk: "\u{1b}[32mOK\u{1b}[0m\n".into(),
        truncated: false,
    }));
    model.apply(BridgeEvent::Update(Box::new(SessionUpdate::ToolCall(
        ToolCall::new("t", "Tests")
            .content(vec![ToolCallContent::Terminal(Terminal::new(id.clone()))]),
    ))));
    insta::assert_json_snapshot!("ui_terminal_tool", wire(&model)["main"]);
    model.apply(BridgeEvent::Terminal(TerminalEvent::Exited {
        id,
        status: TerminalExitStatus::new().exit_code(0),
    }));
    let after = wire(&model);
    assert!(after.to_string().contains("Exit 0"));
    assert!(!after.to_string().contains("Kill terminal"));
}

#[test]
fn elicitation_form_snapshot() {
    let mut model = Model::default();
    model.apply(BridgeEvent::Elicitation {
        id: 3,
        request: Box::new(CreateElicitationRequest::new(
            ElicitationFormMode::new(
                ElicitationSessionScope::new("s"),
                ElicitationSchema::new()
                    .property("name", StringPropertySchema::new().title("Your name"), true)
                    .property("count", IntegerPropertySchema::new().minimum(1), true),
            ),
            "What should I use?",
        )),
    });
    insta::assert_json_snapshot!("ui_elicitation_form", wire(&model)["layer"]);
}
