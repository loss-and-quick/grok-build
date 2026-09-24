use super::*;

#[test]
fn exported_schema_hides_model_only_under_inherited_selection() {
    let mut schema =
        serde_json::to_value(schemars::schema_for!(xai_tool_types::TaskToolInput)).unwrap();
    let has = |schema: &serde_json::Value, name: &str| schema["properties"].get(name).is_some();
    // The fork never advertises `model` on the task tool (the subagent type pins
    // it), so the filter is exercised on a schema that does carry one.
    assert!(!has(&schema, "model"));
    schema["properties"]["model"] = serde_json::json!({ "type": "string" });

    let selectable = exported_input_schema(&schema, TaskModelSelection::Selectable);
    assert!(has(&selectable, "model"));

    let hidden = exported_input_schema(&schema, TaskModelSelection::Inherited);
    assert!(!has(&hidden, "model"));
    assert!(has(&hidden, "prompt"));
}
