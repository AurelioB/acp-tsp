//! ACP content blocks rendered as native TSP nodes.
use std::hash::{Hash, Hasher};

use agent_client_protocol::schema::v1::{ContentBlock, EmbeddedResourceResource, TerminalId};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use tern_sdk::{
    Session,
    ui::{self, Node, html},
};

use super::Msg;
use crate::model::{Model, content::TerminalBuffer};

pub fn image_key(data: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

/// Upload only images not previously sent; `Session::blob` deduplicates wire payloads too.
pub fn upload(term: &mut Session<Msg>, model: &mut Model) -> anyhow::Result<()> {
    if !term
        .caps()
        .features
        .iter()
        .any(|feature| feature == "blobs")
    {
        return Ok(());
    }
    let mut pending = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in &model.transcript {
        match &entry.kind {
            crate::model::EntryKind::User { blocks, .. }
            | crate::model::EntryKind::Steering { blocks }
            | crate::model::EntryKind::Agent { blocks, .. }
            | crate::model::EntryKind::Thought { blocks, .. } => {
                for block in blocks {
                    collect_image(
                        block,
                        &model.content.blobs,
                        &model.content.invalid_images,
                        &mut seen,
                        &mut pending,
                    );
                }
            }
            crate::model::EntryKind::Tool { call, .. } => {
                for item in &call.content {
                    if let agent_client_protocol::schema::v1::ToolCallContent::Content(content) =
                        item
                    {
                        collect_image(
                            &content.content,
                            &model.content.blobs,
                            &model.content.invalid_images,
                            &mut seen,
                            &mut pending,
                        );
                    }
                }
            }
            #[cfg(feature = "unstable_session_compaction")]
            crate::model::EntryKind::Compaction { summary, .. } => {
                for block in summary {
                    collect_image(
                        block,
                        &model.content.blobs,
                        &model.content.invalid_images,
                        &mut seen,
                        &mut pending,
                    );
                }
            }
            #[cfg(feature = "unstable_subagents")]
            crate::model::EntryKind::Subagent { messages, .. } => {
                for message in messages {
                    if let agent_client_protocol::schema::MaybeUndefined::Value(blocks) =
                        &message.content
                    {
                        for block in blocks {
                            collect_image(
                                block,
                                &model.content.blobs,
                                &model.content.invalid_images,
                                &mut seen,
                                &mut pending,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (key, mime, data) in pending {
        match STANDARD.decode(data) {
            Ok(data) => {
                let id = term.blob(data, Some(mime))?;
                model.content.blobs.insert(key, id);
            }
            Err(_) => {
                model.content.invalid_images.insert(key);
                model.errors.push("Agent sent invalid image data".into());
            }
        }
    }
    Ok(())
}

fn collect_image<'a>(
    block: &'a ContentBlock,
    known: &std::collections::HashMap<u64, String>,
    invalid: &std::collections::HashSet<u64>,
    seen: &mut std::collections::HashSet<u64>,
    pending: &mut Vec<(u64, &'a str, &'a str)>,
) {
    if let ContentBlock::Image(image) = block {
        let key = image_key(&image.data);
        if !known.contains_key(&key) && !invalid.contains(&key) && seen.insert(key) {
            pending.push((key, &image.mime_type, &image.data));
        }
    }
}

pub fn block(block: &ContentBlock, key: impl Into<String>, model: &Model) -> Node<Msg> {
    let key = key.into();
    match block {
        ContentBlock::Text(text) => ui::md(text.text.clone()).key(key).into(),
        ContentBlock::Image(image) => {
            if let Some(id) = model.content.blobs.get(&image_key(&image.data)) {
                ui::image()
                    .key(key)
                    .blob(id.clone())
                    .alt("Attached image")
                    .into()
            } else {
                ui::text("Image unavailable").key(key).into()
            }
        }
        ContentBlock::ResourceLink(link) => {
            ui::md(crate::model::resource_link_markdown(&link.name, &link.uri))
                .key(key)
                .into()
        }
        ContentBlock::Resource(resource) => match &resource.resource {
            EmbeddedResourceResource::TextResourceContents(text) => {
                let language = text
                    .uri
                    .rsplit('/')
                    .next()
                    .and_then(|file| file.rsplit_once('.').map(|(_, ext)| ext))
                    .or_else(|| {
                        text.mime_type
                            .as_deref()
                            .and_then(|mime| mime.split('/').next_back())
                            .map(|subtype| {
                                subtype
                                    .split(';')
                                    .next()
                                    .unwrap_or(subtype)
                                    .trim_start_matches("x-")
                            })
                    });
                let mut code = ui::code(text.text.clone()).key("code");
                if let Some(lang) = language {
                    code = code.lang(lang.to_owned());
                }
                ui::section()
                    .key(key)
                    .head(text.uri.clone())
                    .collapsible(true)
                    .collapsed(true)
                    .child(code)
                    .into()
            }
            EmbeddedResourceResource::BlobResourceContents(blob) => ui::badge(format!(
                "Binary resource: {}",
                blob.mime_type.as_deref().unwrap_or("unknown")
            ))
            .key(key)
            .into(),
            _ => ui::badge("Unsupported resource").key(key).into(),
        },
        ContentBlock::Audio(audio) => ui::badge(format!("Audio: {}", audio.mime_type))
            .key(key)
            .into(),
        _ => ui::text("Unsupported content").key(key).into(),
    }
}

pub fn blocks(blocks: &[ContentBlock], prefix: &str, model: &Model) -> Vec<Node<Msg>> {
    blocks
        .iter()
        .enumerate()
        .map(|(index, item)| block(item, format!("{prefix}.{index}"), model))
        .collect()
}

pub fn terminal(
    id: &TerminalId,
    state: Option<&TerminalBuffer>,
    key: impl Into<String>,
) -> Node<Msg> {
    let key = key.into();
    if let Some(state) = state {
        let status = state.status.as_ref().map_or(
            if state.released {
                "Released".to_owned()
            } else {
                "Running".to_owned()
            },
            |status| {
                status.exit_code.map_or_else(
                    || "Exited by signal".to_owned(),
                    |code| format!("Exit {code}"),
                )
            },
        );
        let mut card = ui::section()
            .key(key)
            .head(state.command.clone())
            .collapsible(true)
            .on_focus(Msg::TerminalFocus(id.clone()))
            .child(ui::badge(status).key("status"))
            .child(ui::ansi(state.output.clone()).key("output"));
        if state.truncated {
            card = card.child(ui::text("Earlier output truncated").key("truncated"));
        }
        if !state.released && state.status.is_none() {
            card = card.child(
                html::button("Kill terminal")
                    .key("kill")
                    .on_focus(Msg::TerminalFocus(id.clone()))
                    .on_click(Msg::KillTerminal(id.clone())),
            );
        }
        card.into()
    } else {
        ui::text(format!("Terminal {id}")).key(key).into()
    }
}
