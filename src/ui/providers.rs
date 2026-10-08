//! Unstable provider routing editor. `providers/list` deliberately omits
//! secret headers, so editing an active provider requires entering the full
//! replacement header map before submitting `providers/set`.

use std::collections::HashMap;

use agent_client_protocol::schema::v1::{
    DisableProviderRequest, LlmProtocol, ProviderId, ProviderInfo, SetProviderRequest,
};
use tern_sdk::{
    keys::Key,
    ui::{self, ChoiceOption, ChoiceStyle, Control, PrefsRow, PrefsSection},
};

use crate::event::UiCmd;

use super::Msg;
#[derive(Debug)]
pub struct ProviderEditor {
    id: ProviderId,
    protocols: Vec<LlmProtocol>,
    selected_protocol: usize,
    base_url: String,
    headers: String,
    headers_edited: bool,
    required: bool,
    active: bool,
    selected_row: usize,
    error: Option<String>,
}

impl ProviderEditor {
    pub fn new(provider: &ProviderInfo) -> Self {
        let protocols = provider.supported.clone();
        let selected_protocol = provider
            .current
            .as_ref()
            .and_then(|current| {
                protocols
                    .iter()
                    .position(|protocol| protocol == &current.api_type)
            })
            .unwrap_or_default();
        Self {
            id: provider.provider_id.clone(),
            protocols,
            selected_protocol,
            base_url: provider
                .current
                .as_ref()
                .map(|current| current.base_url.clone())
                .unwrap_or_default(),
            headers: if provider.current.is_some() {
                String::new()
            } else {
                "{}".into()
            },
            headers_edited: provider.current.is_none(),
            required: provider.required,
            active: provider.current.is_some(),
            selected_row: 0,
            error: None,
        }
    }

    pub fn render(&self) -> impl Into<ui::Node<Msg>> {
        let protocol_options = self
            .protocols
            .iter()
            .map(|protocol| {
                let value = protocol_id(protocol);
                ChoiceOption {
                    label: Some(value.clone()),
                    value,
                    detail: None,
                }
            })
            .collect();
        let rows = vec![
            PrefsRow {
                id: "api".into(),
                label: Some("API protocol".into()),
                control: Some(Control::Choice {
                    value: self
                        .protocols
                        .get(self.selected_protocol)
                        .map(protocol_id)
                        .unwrap_or_default(),
                    options: protocol_options,
                    style: Some(ChoiceStyle::Menu),
                    mono: None,
                }),
                ..PrefsRow::default()
            },
            PrefsRow {
                id: "url".into(),
                label: Some("Base URL".into()),
                control: Some(Control::Text {
                    value: self.base_url.clone(),
                    placeholder: Some("https://api.example.com".into()),
                    secret: None,
                    mono: None,
                }),
                ..PrefsRow::default()
            },
            PrefsRow {
                id: "headers".into(),
                label: Some("Full headers JSON".into()),
                hint: Some(
                    "Set the full replacement map; existing secret headers cannot be read back"
                        .into(),
                ),
                control: Some(Control::Text {
                    value: self.headers.clone(),
                    placeholder: Some("{\"Authorization\":\"Bearer …\"}".into()),
                    secret: Some(true),
                    mono: None,
                }),
                ..PrefsRow::default()
            },
            PrefsRow {
                id: "enabled".into(),
                label: Some("Enabled".into()),
                disabled: self
                    .required
                    .then(|| "Required provider cannot be disabled".into()),
                control: Some(Control::Switch { on: self.active }),
                ..PrefsRow::default()
            },
            PrefsRow {
                id: "apply".into(),
                label: Some("Apply routing".into()),
                control: Some(Control::Action {
                    act: Some("apply".into()),
                    label: Some("Save provider".into()),
                }),
                ..PrefsRow::default()
            },
        ];
        let mut page = ui::prefs()
            .key("provider")
            .title(format!("Provider {}", self.id))
            .sections(vec![PrefsSection {
                id: Some("routing".into()),
                title: Some("Routing".into()),
                rows,
                ..PrefsSection::default()
            }])
            .on_change(|change| Msg::ProviderChange(change.item.clone(), change.value.clone()))
            .on_action("apply", Msg::ProviderApply);
        if let Some(error) = &self.error {
            page = page.lead(error.clone());
        }
        page
    }

    pub fn change(&mut self, id: Option<&str>, value: &serde_json::Value) -> Option<UiCmd> {
        self.error = None;
        match id? {
            "api" => {
                let id = value.as_str()?;
                self.selected_protocol =
                    self.protocols.iter().position(|p| protocol_id(p) == id)?;
            }
            "url" => self.base_url = value.as_str()?.to_owned(),
            "headers" => {
                self.headers = value.as_str()?.to_owned();
                self.headers_edited = true;
            }
            "enabled" if self.active && !self.required && value.as_bool() == Some(false) => {
                return Some(UiCmd::DisableProvider(Box::new(
                    DisableProviderRequest::new(self.id.clone()),
                )));
            }
            "enabled" if !self.active && value.as_bool() == Some(true) => {
                self.error = Some("Enter a base URL and save to enable this provider".into());
            }
            _ => return None,
        }
        None
    }

    pub fn apply(&mut self) -> Option<UiCmd> {
        let Some(protocol) = self.protocols.get(self.selected_protocol) else {
            self.error = Some("No supported API protocol".into());
            return None;
        };
        if self.base_url.trim().is_empty() {
            self.error = Some("Enter a base URL".into());
            return None;
        }
        if !self.headers_edited {
            self.error = Some(
                "Enter the full headers JSON; existing secret headers are not available".into(),
            );
            return None;
        }
        let headers: HashMap<String, String> = match serde_json::from_str(&self.headers) {
            Ok(headers) => headers,
            Err(_) => {
                self.error = Some("Headers must be a JSON object of string values".into());
                return None;
            }
        };
        self.error = None;
        Some(UiCmd::SetProvider(Box::new(
            SetProviderRequest::new(self.id.clone(), protocol.clone(), self.base_url.clone())
                .headers(headers),
        )))
    }

    pub fn key(&mut self, key: &Key) -> Option<UiCmd> {
        if key.is("up") {
            self.selected_row = self.selected_row.saturating_sub(1);
            return None;
        }
        if key.is("down") {
            self.selected_row = (self.selected_row + 1).min(4);
            return None;
        }
        match self.selected_row {
            0 if key.is("left") || key.is("right") => {
                if !self.protocols.is_empty() {
                    self.selected_protocol = (self.selected_protocol
                        + if key.is("left") {
                            self.protocols.len() - 1
                        } else {
                            1
                        })
                        % self.protocols.len();
                }
            }
            1 | 2 => {
                let text = if self.selected_row == 1 {
                    &mut self.base_url
                } else {
                    self.headers_edited = true;
                    &mut self.headers
                };
                if key.is("backspace") {
                    text.pop();
                } else if let Some(typed) = key.typed() {
                    text.push_str(typed);
                }
            }
            3 if key.is("enter") && self.active && !self.required => {
                return self.change(Some("enabled"), &serde_json::Value::Bool(false));
            }
            4 if key.is("enter") => return self.apply(),
            _ => {}
        }
        None
    }
}

fn protocol_id(protocol: &LlmProtocol) -> String {
    serde_json::to_value(protocol)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::ProviderCurrentConfig;
    use serde_json::json;

    fn provider(required: bool) -> ProviderInfo {
        ProviderInfo::new(
            "primary",
            vec![LlmProtocol::OpenAi],
            required,
            Some(ProviderCurrentConfig::new(
                LlmProtocol::OpenAi,
                "https://api.example.test",
            )),
        )
    }

    #[test]
    fn editing_active_provider_requires_complete_replacement_headers() {
        let mut editor = ProviderEditor::new(&provider(false));
        assert!(
            editor.apply().is_none(),
            "secret headers cannot be copied silently"
        );
        editor.change(Some("headers"), &json!("not json"));
        assert!(
            editor.apply().is_none(),
            "invalid header maps cannot be sent"
        );
        editor.change(
            Some("headers"),
            &json!(r#"{"Authorization":"Bearer token"}"#),
        );
        let Some(UiCmd::SetProvider(request)) = editor.apply() else {
            panic!("expected providers/set");
        };
        assert_eq!(request.provider_id.0.as_ref(), "primary");
        assert_eq!(request.base_url, "https://api.example.test");
        assert_eq!(
            request.headers.get("Authorization").map(String::as_str),
            Some("Bearer token")
        );
    }

    #[test]
    fn required_provider_cannot_be_disabled() {
        let mut required = ProviderEditor::new(&provider(true));
        assert!(required.change(Some("enabled"), &json!(false)).is_none());
        let mut optional = ProviderEditor::new(&provider(false));
        assert!(matches!(
            optional.change(Some("enabled"), &json!(false)),
            Some(UiCmd::DisableProvider(_))
        ));
    }
}
