use std::collections::BTreeMap;

use acp_tsp::{
    event::BridgeEvent,
    model::Model,
    ui::elicit::{FormState, validate},
};
use agent_client_protocol::schema::v1::{
    BooleanPropertySchema, CreateElicitationRequest, ElicitationContentValue as Value,
    ElicitationFormMode, ElicitationSchema, ElicitationSessionScope, ElicitationUrlMode,
    IntegerPropertySchema, NumberPropertySchema, PermissionOption, PermissionOptionKind,
    RequestPermissionRequest, StringPropertySchema, ToolCallUpdate, ToolCallUpdateFields,
};

fn form() -> CreateElicitationRequest {
    CreateElicitationRequest::new(
        ElicitationFormMode::new(
            ElicitationSessionScope::new("s"),
            ElicitationSchema::new()
                .property(
                    "name",
                    StringPropertySchema::new().min_length(2).max_length(8),
                    true,
                )
                .property(
                    "count",
                    IntegerPropertySchema::new().minimum(1).maximum(10),
                    true,
                )
                .property(
                    "ratio",
                    NumberPropertySchema::new().minimum(0.0).maximum(1.0),
                    false,
                )
                .property(
                    "enabled",
                    BooleanPropertySchema::new().default_value(false),
                    false,
                ),
        ),
        "Enter details",
    )
}

#[test]
fn validation_rejects_required_invalid_bounds_and_numeric_types() {
    let request = form();
    let agent_client_protocol::schema::v1::ElicitationMode::Form(form) = &request.mode else {
        panic!("form")
    };
    let mut values = BTreeMap::new();
    assert_eq!(validate(&form.requested_schema, &values).len(), 2);
    values.insert("name".into(), Value::String("A".into()));
    values.insert("count".into(), Value::String("2.5".into()));
    values.insert("ratio".into(), Value::String("nan".into()));
    let errors = validate(&form.requested_schema, &values);
    assert_eq!(errors["name"], "Too short");
    assert_eq!(errors["count"], "Enter an integer");
    assert_eq!(errors["ratio"], "Enter a finite number");
    values.insert("name".into(), Value::String("Ada".into()));
    values.insert("count".into(), Value::String("11".into()));
    values.insert("ratio".into(), Value::String("1.1".into()));
    assert_eq!(validate(&form.requested_schema, &values).len(), 2);
    values.insert("count".into(), Value::String("10".into()));
    values.insert("ratio".into(), Value::String("0.75".into()));
    assert!(validate(&form.requested_schema, &values).is_empty());
    let mut state = FormState::default();
    state.init(5, &request);
    state.values.extend(values);
    let Some(acp_tsp::event::UiCmd::Elicitation { response, .. }) = state.accept(5, &request)
    else {
        panic!("accepted")
    };
    let agent_client_protocol::schema::v1::ElicitationAction::Accept(accepted) = response.action
    else {
        panic!("accept")
    };
    let content = accepted.content.unwrap();
    assert_eq!(content["count"], Value::Integer(10));
    assert_eq!(content["ratio"], Value::Number(0.75));
    assert_eq!(content["enabled"], Value::Boolean(false));
}

#[test]
fn elicitation_permission_fifo_and_url_completion_resolve_by_id() {
    let mut model = Model::default();
    let url = CreateElicitationRequest::new(
        ElicitationUrlMode::new(
            ElicitationSessionScope::new("s"),
            "url1",
            "https://example.test/login",
        ),
        "Sign in",
    );
    model.apply(BridgeEvent::Elicitation {
        id: 4,
        request: Box::new(url),
    });
    model.apply(BridgeEvent::Permission {
        id: 5,
        request: Box::new(RequestPermissionRequest::new(
            "s",
            ToolCallUpdate::new("t", ToolCallUpdateFields::new()),
            vec![PermissionOption::new(
                "yes",
                "Allow",
                PermissionOptionKind::AllowOnce,
            )],
        )),
    });
    model.apply(BridgeEvent::Elicitation {
        id: 6,
        request: Box::new(form()),
    });
    assert_eq!(model.oldest_elicitation().unwrap().id, 4);
    assert!(model.oldest_permission().is_none());
    model.apply(BridgeEvent::ElicitationComplete("url1".into()));
    assert_eq!(model.oldest_permission().unwrap().id, 5);
    model.apply(BridgeEvent::Resolved(5));
    assert_eq!(model.oldest_elicitation().unwrap().id, 6);
    model.apply(BridgeEvent::Resolved(6));
    assert!(model.dialogs.is_empty());
}
