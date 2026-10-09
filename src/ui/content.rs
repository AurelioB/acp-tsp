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

/// Forget image references after the surface is replaced so upload repopulates the view.
/// The SDK retains blobs across surfaces and deduplicates the wire payload by hash.
pub fn reset_uploads(model: &mut Model) {
    model.content.blobs.clear();
}

/// Upload only images not previously sent; `Session::blob` deduplicates wire payloads too.
pub fn upload(
    term: &mut Session<Msg>,
    model: &mut Model,
    composer: &mut super::composer::Composer,
) -> anyhow::Result<()> {
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
    for image in &mut composer.images {
        if image.blob.is_none() {
            image.blob = Some(term.blob(image.bytes.clone(), Some(&image.mime_type))?);
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

/// JSON keeps a native browsable view and an independently foldable source view.
/// Tern copies the source from the code node, not the tree/table presentation.
pub(crate) fn json_display(
    value: &serde_json::Value,
    source: &str,
    key: &str,
    model: &Model,
    caps: &tern_sdk::Capabilities,
    scope: &str,
) -> Node<Msg> {
    let source_key = format!("{scope}.{key}.source");
    let code = ui::section()
        .key("source")
        .head("Raw JSON")
        .collapsible(true)
        .collapsed(model.folds.get(&source_key).copied().unwrap_or(true))
        .on_toggle(move |collapsed| Msg::ToggleFold(source_key.clone(), collapsed))
        .child(
            ui::code(source.to_owned())
                .key("code")
                .lang("json")
                .actions(copy_menu()),
        );
    let mut col = ui::col().key(key);
    if let Some(data) = json_data(value, caps) {
        col = col.child(data);
    }
    col.child(code).into()
}

pub(crate) fn json_data(
    value: &serde_json::Value,
    caps: &tern_sdk::Capabilities,
) -> Option<Node<Msg>> {
    if let Some(rows) = value.as_array()
        && !rows.is_empty()
        && rows.len() <= 200
        && rows.iter().all(|row| {
            row.as_object()
                .is_some_and(|obj| obj.values().all(is_scalar))
        })
        && super::supports(caps, "table")
    {
        let mut names = Vec::new();
        'columns: for row in rows {
            for name in row.as_object().expect("all rows are objects").keys() {
                if !names.contains(name) {
                    names.push(name.clone());
                    if names.len() > 8 {
                        break 'columns;
                    }
                }
            }
        }
        if !names.is_empty() && names.len() <= 8 {
            let cols: Vec<_> = names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    ui::Column::new(name.clone(), name.clone()).priority((names.len() - i) as f64)
                })
                .collect();
            let rows: Vec<_> = rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let mut out = ui::TableRow::new(i.to_string());
                    for (name, value) in row.as_object().expect("all rows are objects") {
                        out = out.cell(name.clone(), scalar(value));
                    }
                    out
                })
                .collect();
            return Some(ui::table().key("data").cols(cols).rows(rows).into());
        }
    }
    if !super::supports(caps, "tree") || !value.is_object() && !value.is_array() {
        return None;
    }
    let mut remaining = 498;
    let mut truncated = false;
    let root = tree_item(
        value,
        "#".into(),
        "JSON".into(),
        &mut remaining,
        &mut truncated,
    )
    .open(true);
    Some(ui::tree().key("data").nodes(vec![root]).into())
}

fn is_scalar(value: &serde_json::Value) -> bool {
    !value.is_object() && !value.is_array()
}

fn scalar(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn tree_item(
    value: &serde_json::Value,
    path: String,
    label: String,
    remaining: &mut usize,
    truncated: &mut bool,
) -> ui::TreeItem {
    match value {
        serde_json::Value::Object(object) => tree_children(
            ui::TreeItem::new(path.clone(), format!("{label} {{{}}}", object.len())),
            &path,
            object.len(),
            object.iter().map(|(key, value)| (key.as_str(), value)),
            remaining,
            truncated,
        ),
        serde_json::Value::Array(array) => tree_children(
            ui::TreeItem::new(path.clone(), format!("{label} [{}]", array.len())),
            &path,
            array.len(),
            array
                .iter()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value)),
            remaining,
            truncated,
        ),
        _ => ui::TreeItem::new(path, label),
    }
}

fn tree_children<'a, S: AsRef<str>, I: Iterator<Item = (S, &'a serde_json::Value)>>(
    mut node: ui::TreeItem,
    path: &str,
    count: usize,
    items: I,
    remaining: &mut usize,
    truncated: &mut bool,
) -> ui::TreeItem {
    for (index, (name, child)) in items.enumerate() {
        if *remaining == 0 {
            if !*truncated {
                node.children.push(ui::TreeItem::new(
                    format!("{path}/…"),
                    format!("… {} more", count - index),
                ));
                *truncated = true;
            }
            break;
        }
        *remaining -= 1;
        let name = name.as_ref();
        let child_path = format!("{path}/{}", name.replace('~', "~0").replace('/', "~1"));
        let child_label = if is_scalar(child) {
            format!("{name}: {}", scalar(child))
        } else {
            name.to_owned()
        };
        node.children.push(tree_item(
            child,
            child_path,
            child_label,
            remaining,
            truncated,
        ));
    }
    node
}

pub(crate) fn copy_menu() -> ui::Actions {
    ui::Actions::new().menu("copy")
}

pub fn block(
    block: &ContentBlock,
    key: impl Into<String>,
    model: &Model,
    caps: &tern_sdk::Capabilities,
    scope: &str,
) -> Node<Msg> {
    let key = key.into();
    match block {
        ContentBlock::Text(text) => {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text.text)
                && (value.is_object() || value.is_array())
                && (super::supports(caps, "tree") || super::supports(caps, "table"))
            {
                json_display(&value, &text.text, &key, model, caps, scope)
            } else {
                ui::md(line_breaks(&text.text))
                    .key(key)
                    .actions(copy_menu())
                    .into()
            }
        }
        ContentBlock::Image(image) => {
            if let Some(id) = model.content.blobs.get(&image_key(&image.data)) {
                ui::image()
                    .key(key)
                    .blob(id.clone())
                    .alt("Attached image")
                    .actions(ui::Actions::new().click("zoom"))
                    .into()
            } else {
                ui::text("Image unavailable").key(key).into()
            }
        }
        ContentBlock::ResourceLink(link) => resource_link(&link.name, &link.uri, key).into(),
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
                let mut code = ui::code(text.text.clone()).key("code").actions(copy_menu());
                if let Some(lang) = language {
                    code = code.lang(lang.to_owned());
                }
                let fold_key = format!("{scope}.{key}");
                let mut section = ui::section()
                    .key(key)
                    .head(text.uri.clone())
                    .collapsible(true)
                    .collapsed(model.folds.get(&fold_key).copied().unwrap_or(true))
                    .on_toggle(move |collapsed| Msg::ToggleFold(fold_key.clone(), collapsed));
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text.text)
                    && (value.is_object() || value.is_array())
                    && let Some(data) = json_data(&value, caps)
                {
                    section = section.child(data);
                }
                section.child(code).into()
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

pub fn blocks(
    blocks: &[ContentBlock],
    prefix: &str,
    model: &Model,
    caps: &tern_sdk::Capabilities,
    scope: &str,
) -> Vec<Node<Msg>> {
    blocks
        .iter()
        .enumerate()
        .map(|(index, item)| block(item, format!("{prefix}.{index}"), model, caps, scope))
        .collect()
}

/// Attachment links form chips in the user bubble.
pub fn user_blocks(
    blocks: &[ContentBlock],
    model: &Model,
    caps: &tern_sdk::Capabilities,
    scope: &str,
    edit_id: u64,
) -> Vec<Node<Msg>> {
    let mut body = Vec::new();
    let mut attachments = ui::row()
        .key("attachments")
        .role("omp.user.images")
        .gap(ui::Gap::Sm)
        .wrap(true);
    let mut attached = false;
    for (i, block) in blocks.iter().enumerate() {
        match block {
            ContentBlock::ResourceLink(link) => {
                attachments = attachments.child(
                    resource_link(&link.name, &link.uri, format!("attachment.{i}"))
                        .role("omp.user.attachment"),
                );
                attached = true;
            }
            ContentBlock::Text(_) => {
                let node = self::block(block, format!("body.{i}"), model, caps, scope);
                body.push(
                    node.actions(ui::Actions::new().menu("copy").menu("edit"))
                        .on_menu("edit", Msg::EditPrompt(edit_id)),
                );
            }
            other => body.push(self::block(other, format!("body.{i}"), model, caps, scope)),
        }
    }
    if attached {
        body.insert(0, attachments.into());
    }
    body
}

/// Agents (and their own UIs) treat a single newline in prose as a line break; Markdown would
/// join the lines into one paragraph ("1\n2\n3" as "1 2 3"). Marks each such newline as a
/// hard break, outside code fences and display math, so the text keeps its lines.
pub(crate) fn line_breaks(markdown: &str) -> std::borrow::Cow<'_, str> {
    if !markdown.contains('\n') {
        return markdown.into();
    }
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut out = String::with_capacity(markdown.len() + lines.len() * 2);
    let mut fence: Option<(char, usize)> = None;
    let mut math = false;
    for (i, line) in lines.iter().enumerate() {
        // Fences inside quotes count too (`> ```rust`).
        let trimmed = line.trim_start().trim_start_matches(['>', ' ']);
        let opener = trimmed
            .chars()
            .next()
            .filter(|c| *c == '`' || *c == '~')
            .map(|c| (c, trimmed.chars().take_while(|d| *d == c).count()))
            .filter(|(_, n)| *n >= 3);
        let mut literal = fence.is_some() || math;
        match (fence, opener) {
            (None, Some(open)) if !math => {
                fence = Some(open);
                literal = true;
            }
            (Some((c, n)), Some((d, m)))
                if c == d && m >= n && trimmed.trim_end().chars().all(|x| x == c) =>
            {
                fence = None;
            }
            _ => {}
        }
        if fence.is_none() && trimmed.trim_end() == "$$" {
            math = !math;
            literal = true;
        }
        out.push_str(line);
        if i + 1 < lines.len() {
            let next = lines[i + 1].trim();
            // Indented lines may be code: never touched.
            let indented = line.starts_with('\t') || line.starts_with("    ");
            let breaks = !literal
                && !indented
                && fence.is_none()
                && !math
                && !line.trim().is_empty()
                && !next.is_empty()
                && !line.ends_with("  ")
                && !line.ends_with('\\')
                && !next.starts_with("```")
                && !next.starts_with("~~~")
                && next != "$$";
            if breaks {
                out.push_str("  ");
            }
            out.push('\n');
        }
    }
    out.into()
}

fn link_href(uri: &str) -> Option<&str> {
    (uri.starts_with("file://") || uri.starts_with("http://") || uri.starts_with("https://"))
        .then_some(uri)
}

fn resource_link(name: &str, uri: &str, key: String) -> ui::Md<Msg> {
    let mut node = ui::md(crate::model::resource_link_markdown(name, uri)).key(key);
    if let Some(href) = link_href(uri) {
        node = node
            .href(href)
            .actions(ui::Actions::new().menu("open").menu("copy"));
    }
    node
}

pub fn terminal(
    id: &TerminalId,
    state: Option<&TerminalBuffer>,
    key: impl Into<String>,
    model: &Model,
    scope: &str,
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
        let fold_key = format!("{scope}.{key}");
        let mut card = ui::section()
            .key(key)
            .head(state.command.clone())
            .collapsible(true)
            .collapsed(model.folds.get(&fold_key).copied().unwrap_or(false))
            .on_toggle(move |collapsed| Msg::ToggleFold(fold_key.clone(), collapsed))
            .on_focus(Msg::TerminalFocus(id.clone()))
            .child(ui::badge(status).key("status"))
            .child(
                ui::ansi(state.output.clone())
                    .key("output")
                    .actions(copy_menu())
                    .prop("max", serde_json::json!({"h": "12lines"})),
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn data(value: Value) -> Value {
        let caps = tern_sdk::Capabilities {
            kinds: vec!["tree".into(), "table".into()],
            ..Default::default()
        };
        let node = json_data(&value, &caps).expect("structured JSON has a native view");
        let doc = tern_sdk::reconcile::Doc::from_view(ui::View::new().main(ui::col().child(node)))
            .unwrap();
        serde_json::to_value(doc.regions()[0].as_ref().unwrap().wire()).unwrap()["c"][0].clone()
    }

    #[test]
    fn nested_json_uses_stable_pointer_ids_and_scalar_labels() {
        let node = data(json!({"a/b": {"~": [true, null]}}));
        assert_eq!(node["k"], "tree");
        let root = &node["p"]["nodes"][0];
        assert_eq!(root["id"], "#");
        assert_eq!(root["open"], true);
        assert_eq!(root["children"][0]["id"], "#/a~1b");
        assert_eq!(root["children"][0]["label"], "a/b {1}");
        assert_eq!(root["children"][0]["children"][0]["id"], "#/a~1b/~0");
        assert_eq!(
            root["children"][0]["children"][0]["children"][0]["label"],
            "0: true"
        );
    }

    #[test]
    fn json_table_boundaries_and_mixed_arrays_fall_back_to_tree() {
        let rows: Vec<_> = (0..200)
            .map(|i| json!({"first": i, "second": null}))
            .collect();
        let table = data(json!(rows));
        assert_eq!(table["k"], "table");
        assert_eq!(table["p"]["rows"].as_array().unwrap().len(), 200);
        assert_eq!(table["p"]["rows"][199]["id"], "199");
        assert!(
            table["p"]["cols"][0]["priority"].as_f64().unwrap()
                > table["p"]["cols"][1]["priority"].as_f64().unwrap()
        );
        assert_eq!(data(json!([{"x": 1}, {"x": [2]}]))["k"], "tree");
        assert_eq!(
            data(json!((0..201).map(|i| json!({"x": i})).collect::<Vec<_>>()))["k"],
            "tree"
        );
        assert_eq!(
            data(json!([{"a":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7,"h":8,"i":9}]))["k"],
            "tree"
        );
    }

    #[test]
    fn large_json_tree_limits_rows_and_marks_omitted_items() {
        let node = data(json!({"items": (0..600).collect::<Vec<_>>()}));
        let items = &node["p"]["nodes"][0]["children"][0]["children"];
        assert!(items.as_array().unwrap().len() <= 500);
        assert!(
            items.as_array().unwrap().last().unwrap()["label"]
                .as_str()
                .unwrap()
                .contains("more")
        );
    }
}

#[cfg(test)]
mod line_break_tests {
    use super::line_breaks;

    #[test]
    fn single_newlines_in_prose_become_hard_breaks() {
        assert_eq!(line_breaks("1\n2\n3"), "1  \n2  \n3");
        assert_eq!(
            line_breaks("para one\n\npara two"),
            "para one\n\npara two",
            "blank lines still separate paragraphs"
        );
        assert_eq!(
            line_breaks("a  \nb\\\nc"),
            "a  \nb\\\nc",
            "existing breaks kept"
        );
        assert_eq!(line_breaks("one line"), "one line");
    }

    #[test]
    fn code_fences_and_display_math_are_left_alone() {
        let source = "text\n```rust\nlet a = 1;\nlet b = 2;\n```\nafter\n$$\nx = 1\ny = 2\n$$\nend";
        assert_eq!(
            line_breaks(source),
            "text\n```rust\nlet a = 1;\nlet b = 2;\n```\nafter\n$$\nx = 1\ny = 2\n$$\nend"
        );
        let tilde = "~~~~\na\n~~~\nb\n~~~~\nc\nd";
        assert_eq!(
            line_breaks(tilde),
            "~~~~\na\n~~~\nb\n~~~~\nc  \nd",
            "a shorter fence line does not close a longer fence"
        );
    }

    #[test]
    fn indented_code_quoted_fences_and_unclosed_fences_are_left_alone() {
        assert_eq!(
            line_breaks(
                "text:

    let a = 1;
    let b = 2;

after"
            ),
            "text:\n\n    let a = 1;\n    let b = 2;\n\nafter"
        );
        assert_eq!(
            line_breaks("> ```sh\n> make\n> test\n> ```\n> done\n> now"),
            "> ```sh\n> make\n> test\n> ```\n> done  \n> now"
        );
        // Mid-stream: the fence has not closed yet.
        assert_eq!(line_breaks("a\n```\nx\ny"), "a\n```\nx\ny");
    }
}
