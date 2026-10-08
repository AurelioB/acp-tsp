//! Permission and plan approvals, in the ask-dialog vocabulary used by Tern's omp theme.

use agent_client_protocol::schema::v1::{RequestPermissionRequest, ToolCallContent};
use tern_sdk::ui::{self, Node};

use super::{Msg, ext};

pub fn dialog(id: u64, request: &RequestPermissionRequest) -> Node<Msg> {
    let call = &request.tool_call.fields;
    let plan = ext::plan_text(request);
    let title = if plan.is_some() {
        "Plan review".to_owned()
    } else {
        call.title
            .clone()
            .unwrap_or_else(|| "Tool permission".into())
    };
    let mut body = ui::col()
        .role("omp.editor")
        .key("permission")
        .child(ui::text(title).key("title").role("omp.ask.question"));
    if let Some(target) = call
        .locations
        .as_ref()
        .and_then(|locations| locations.first())
    {
        body = body.child(
            ui::text(target.path.display().to_string())
                .key("target")
                .tone("muted"),
        );
    }
    if let Some(plan) = plan {
        body = body.child(ui::md(plan.to_owned()).key("plan"));
    } else if let Some(input) = &call.raw_input {
        // Tool requests are untrusted; represent structured parameters as preview text,
        // without executing or interpreting command arguments.
        let preview = input
            .get("command")
            .or_else(|| input.get("file_path"))
            .or_else(|| input.get("path"))
            .or_else(|| input.get("diff"))
            .and_then(serde_json::Value::as_str);
        if let Some(preview) = preview {
            body = body.child(ui::code(preview).key("preview"));
        }
    }
    if let Some(content) = &call.content {
        for (index, item) in content.iter().enumerate() {
            if let ToolCallContent::Diff(diff) = item {
                body = body.child(
                    ui::col()
                        .key(format!("diff.{index}"))
                        .child(ui::text(diff.path.display().to_string()).tone("muted"))
                        .child(ui::code(diff.new_text.clone()).key("after")),
                );
            }
        }
    }
    let mut options = ui::list()
        .key("options")
        .role("omp.ask.options")
        .selected("layer.permission.sheet.permission.options.option.0");
    for (i, option) in request.options.iter().enumerate() {
        options = options.child(
            ui::item(option.name.clone())
                .key(format!("option.{i}"))
                .role("omp.ask.option")
                .hint(vec![(i + 1).to_string()])
                .on_click(Msg::Permission(id, option.option_id.clone())),
        );
    }
    body = body.child(options).child(
        ui::row()
            .key("actions")
            .role("omp.actions")
            .child(ui::spacer().prop("grow", 1))
            .child(
                ui::row()
                    .key("cancel")
                    .role("omp.btn")
                    .on_click(Msg::PermissionDismiss(id))
                    .child(ui::text("Cancel"))
                    .child(ui::kbd(["escape"])),
            ),
    );
    ui::col()
        .child(
            ui::overlay()
                .key("permission.sheet")
                .anchor(ui::Anchor::Bottom)
                .size(ui::OverlaySize::Lg)
                .modal(true)
                .child(body),
        )
        .into()
}
