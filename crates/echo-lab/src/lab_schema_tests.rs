use super::*;
use serde_json::json;

fn scenario() -> Value {
    serde_norway::from_str(include_str!("../../../tests/scenarios/m0-harness.yaml")).unwrap()
}

#[test]
fn scenario_shapes_and_pointer_diagnostics() {
    let validator = schema().unwrap();
    let original = scenario();
    assert!(validator.is_valid(&original));
    for (key, value) in [
        ("tiers", json!(["T9"])),
        ("setup", json!("spoof")),
        ("steps", json!([{"unknown": true}])),
        ("steps", json!([{"say": "hi", "press": "mute"}])),
        ("negative", json!([{"inject": {"mcp": "http_500"}}])),
        ("budgets", json!({"latency": -1})),
    ] {
        let mut invalid = original.clone();
        invalid[key] = value;
        assert!(!validator.is_valid(&invalid), "{key}: {invalid}");
    }
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("milestone");
    let mut output = Vec::new();
    assert!(!validate_value(&validator, &missing, "missing.yaml", &mut output).unwrap());
    assert!(
        String::from_utf8(output)
            .unwrap()
            .starts_with("missing.yaml: /milestone:")
    );
    let mut escaped = original;
    escaped["budgets"] = json!({"a~/b": "slow"});
    let mut output = Vec::new();
    assert!(!validate_value(&validator, &escaped, "bad.yaml", &mut output).unwrap());
    assert!(String::from_utf8(output).unwrap().contains("/budgets/a~0~1b:"));
}

#[test]
fn all_step_forms_and_negative_assertions_validate() {
    let example: Value = serde_norway::from_str(include_str!("../tests/lab_m4.yaml")).unwrap();
    assert!(schema().unwrap().is_valid(&example));
    let mut value = scenario();
    value["steps"] = json!([
        {"say": "Alexa, call the test tool and tell me its result"},
        {"expect_trace": ["session.delegation.created", "tools/call {name: test.echo, count: 1}", "session.commentary.append"]},
        {"expect_speech_contains": ["test", "echo"]},
        {"expect_screen": {"state": "speaking"}},
        {"expect_frame": "frames/ring.jpg"},
        {"press": "mute"}, {"press": {"key": "mute"}}, {"touch": {"x": 1, "y": 2}}
    ]);
    value["setup"] = json!({"mcp": "spoof", "backend": "client"});
    value["budgets"] = json!({"first_audio_after_endpoint_ms_p95": 1500});
    value["negative"] = json!([{
        "inject": {"mcp": "http_500"}, "expect_speech_contains": ["couldn't"],
        "expect_trace_absent": ["tools/call {count: 2}"]
    }]);
    assert!(schema().unwrap().is_valid(&value));
    let encoded = serde_json::to_string(&value).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), value);
}

#[test]
fn frame_contract_and_semantic_paths() {
    let source: Value = serde_json::from_str(SCHEMA).unwrap();
    assert_eq!(
        source["$defs"]["step"]["properties"]["expect_frame"],
        json!({"type":"string","minLength":1})
    );
    let mut value = scenario();
    value["steps"] = json!([{"expect_frame":"frames/ring.jpg"}]);
    assert!(schema().unwrap().is_valid(&value));
    assert_eq!(frame_paths(&value), Ok(()));
    for path in [
        "",
        "../escape.jpg",
        "/tmp/frame.jpg",
        "C:/frame.jpg",
        "a/../../escape.jpg",
        "a\\frame.jpg",
    ] {
        value["steps"] = json!([{"expect_frame":path}]);
        assert_eq!(
            frame_paths(&value).unwrap_err(),
            "/steps/0/expect_frame: expect_frame: expected a relative artifact path without traversal"
        );
    }
    value["steps"] = json!([{"expect_frame":""}]);
    assert!(!schema().unwrap().is_valid(&value));
    value["steps"] = json!([{"expect_frame":"frame.jpg", "say":"hello"}]);
    assert!(!schema().unwrap().is_valid(&value));
}

#[test]
fn report_contract_preserves_extensions_and_manual_observations() {
    let source: Value = serde_json::from_str(include_str!("../../../schemas/report.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&source).unwrap();
    let report = json!({
        "milestone": "M0", "result": "pass", "placement": "bridge_pc",
        "build_commit": "fixture", "device": null, "evidence": [], "checks": {"schema_valid": true},
        "limitations": [], "tier": "T0", "driver": "sim", "scenario_digest": "fixture",
        "hassmic_commit": null, "bench_session_ids": []
    });
    assert!(validator.is_valid(&report));
    for field in source["required"].as_array().unwrap() {
        let mut missing = report.clone();
        missing.as_object_mut().unwrap().remove(field.as_str().unwrap());
        assert!(!validator.is_valid(&missing), "missing {field}");
    }
    let mut invalid = report.clone();
    invalid["placement"] = json!("on_echo");
    assert!(!validator.is_valid(&invalid));
    let mut manual = report;
    manual["result"] = json!("manual_observation");
    assert!(!validator.is_valid(&manual));
    manual["manual_observation"] = json!({"observer": "operator", "observation": "fixture only"});
    assert!(validator.is_valid(&manual));
    manual["manual_observation"].as_object_mut().unwrap().remove("observer");
    assert!(!validator.is_valid(&manual));
}
