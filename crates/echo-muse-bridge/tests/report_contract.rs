#![cfg(test)]
pub mod support;
use std::path::Path;

use echo_agent_api::AgentBackend;
use echo_muse_bridge::{InMemoryStore, MuseBackend, ReportError, handler_output, report_command_spec};
use serde_json::{Value, json};

use crate::support::{FakeClock, RecordingGadget, accepted, completed, progress, report, task};

#[tokio::test]
async fn strict_parser_rejects_exact_serde_reasons_without_mutation() {
    let backend = MuseBackend::new(
        RecordingGadget::default(),
        FakeClock::default(),
        InMemoryStore::default(),
    );
    let id = backend.submit(task()).await.unwrap();
    let cases = [
        ("{}".to_string(), "missing field `task_id` at line 1 column 2"),
        ("{".to_string(), "EOF while parsing an object at line 1 column 1"),
        (
            r#"{"task_id":"","status":"completed","summary":"x"}"#.into(),
            "correlation id must not be empty at line 1 column 13",
        ),
        (
            r#"{"task_id":1,"status":"completed","summary":"x"}"#.into(),
            "invalid type: integer `1`, expected a string at line 1 column 12",
        ),
        (
            r#"{"task_id":"x","status":"success","summary":"x"}"#.into(),
            "unknown variant `success`, expected `completed` or `failed` at line 1 column 33",
        ),
        (
            r#"{"task_id":"x","status":"completed","summary":"x","extra":true}"#.into(),
            "unknown field `extra`, expected one of `task_id`, `status`, `summary` at line 1 column 57",
        ),
        (
            r#"{"task_id":"x","task_id":"x","status":"completed","summary":"x"}"#.into(),
            "duplicate field `task_id` at line 1 column 24",
        ),
    ];
    for (input, reason) in cases {
        assert_eq!(
            backend.report_intake().accept(&input).unwrap_err().to_string(),
            format!("invalid report: {reason}")
        );
    }
    assert_eq!(
        backend.report_intake().accept(&report(&id, "completed")).unwrap(),
        accepted(&id)
    );
    assert_eq!(backend.next_event(&id).await, Some(progress(&id)));
    assert_eq!(backend.next_event(&id).await, Some(completed(&id)));
}

#[test]
fn handler_maps_accepted_json_and_every_rejection_to_sdk_output() {
    let value = json!({"accepted": true, "task_id": "task-test"});
    assert_eq!(
        handler_output(Ok(value)),
        (r#"{"accepted":true,"task_id":"task-test"}"#.into(), String::new(), 0)
    );
    for (error, reason) in [
        (ReportError::Unknown, "unknown task_id"),
        (ReportError::Expired, "expired task_id"),
        (ReportError::Cancelled, "cancelled task_id"),
        (ReportError::Reported, "task_id already reported"),
    ] {
        assert_eq!(handler_output(Err(error)), (String::new(), reason.into(), 1));
    }
    let invalid = serde_json::from_str::<Value>("{").unwrap_err();
    let reason = format!("invalid report: {invalid}");
    assert_eq!(
        handler_output(Err(ReportError::Invalid(invalid))),
        (String::new(), reason, 1)
    );
}

#[test]
fn only_dotted_result_command_matches_golden_bytes_and_value() {
    let (name, spec) = report_command_spec(Path::new("/opt/echo/bin/echo-muse-report"));
    let golden = include_bytes!("fixtures/echo.report.result.json");
    assert_eq!(name, "echo.report.result.json");
    assert_eq!(serde_json::to_vec(&spec).unwrap(), golden);
    assert_eq!(spec, serde_json::from_slice::<Value>(golden).unwrap());
    let command = name.strip_suffix(".json").unwrap();
    assert!(
        command
            .split('.')
            .all(|word| !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_lowercase()))
    );
    for prefix in ["system.", "file.", "device.", "link."] {
        assert!(!command.starts_with(prefix));
    }
    let (second_name, second) = report_command_spec(Path::new("other-handler"));
    let mut expected = spec;
    expected["exec"] = json!(["other-handler"]);
    assert_eq!((second_name, second), (name, expected));
}
