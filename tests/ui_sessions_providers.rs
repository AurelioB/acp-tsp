#![cfg(feature = "unstable_llm_providers")]

use acp_tsp::{
    event::BridgeEvent,
    model::Model,
    ui::palette::{Kind, Palette},
};
use agent_client_protocol::schema::v1::{
    ListProvidersResponse, LlmProtocol, ProviderCurrentConfig, ProviderInfo,
};
use tern_sdk::{reconcile::Doc, ui::View};

fn active(required: bool) -> Model {
    let mut model = Model::default();
    model.apply(BridgeEvent::Providers(Box::new(
        ListProvidersResponse::new(vec![ProviderInfo::new(
            "primary",
            vec![LlmProtocol::OpenAi],
            required,
            Some(ProviderCurrentConfig::new(
                LlmProtocol::OpenAi,
                "https://api.example.test",
            )),
        )]),
    )));
    model
}

#[test]
fn provider_prefs_snapshot() {
    let model = active(false);
    let mut picker = Palette::new(Kind::Providers);
    picker.select("provider:0", &model, true);
    let doc = Doc::from_view(View::new().layer(picker.render(&model))).unwrap();
    insta::assert_json_snapshot!(
        "ui_provider_prefs",
        serde_json::to_value(doc.regions()[2].as_ref().unwrap().wire()).unwrap()
    );
}
