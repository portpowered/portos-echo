use crate::*;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::fmt::Debug;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T, expected: Value) {
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
    assert_eq!(&serde_json::from_value::<T>(expected).unwrap(), value);
}

fn task_json() -> Value {
    json!({"delegation_id": "d-1", "instructions": "echo", "context": {"nested": [null, true, 1]},
        "deadline_seconds": 60, "max_tool_calls": 8})
}

#[test]
fn contracts_r1_exact_values_round_trip() {
    round_trip(
        &Task {
            delegation_id: DelegationId::new("d-1").unwrap(),
            instructions: "echo".into(),
            context: json!({"nested": [null, true, 1]}),
            deadline_seconds: 60,
            max_tool_calls: 8,
        },
        task_json(),
    );
    round_trip(
        &ToolSpec {
            name: "test.echo".into(),
            description: "Echo".into(),
            input_schema: json!({"properties": {"message": {"type": "string"}}}),
        },
        json!({"name": "test.echo", "description": "Echo",
        "input_schema": {"properties": {"message": {"type": "string"}}}}),
    );
    for is_error in [true, false] {
        round_trip(
            &ToolResult {
                content: json!([{"opaque": null}, false]),
                is_error,
            },
            json!({"content": [{"opaque": null}, false], "is_error": is_error}),
        );
    }
}

#[test]
fn contracts_r2_reject_malformed_task_and_preserve_zero_caps() {
    for (field, wrong) in [
        ("delegation_id", json!("")),
        ("instructions", json!(3)),
        ("deadline_seconds", json!(-1)),
        ("max_tool_calls", json!(4_294_967_296_u64)),
    ] {
        let mut value = task_json();
        value[field] = wrong;
        assert!(serde_json::from_value::<Task>(value).is_err(), "{field}");
    }
    // JSON Value fields accept null and are deliberately opaque.
    for field in ["delegation_id", "instructions", "deadline_seconds", "max_tool_calls"] {
        let mut value = task_json();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<Task>(value).is_err(), "{field}");
    }
    let mut value = task_json();
    value["deadline_seconds"] = json!(0);
    value["max_tool_calls"] = json!(0);
    let task: Task = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(task.deadline_seconds, 0);
    assert_eq!(task.max_tool_calls, 0);
    assert_eq!(serde_json::to_value(task).unwrap(), value);
}
