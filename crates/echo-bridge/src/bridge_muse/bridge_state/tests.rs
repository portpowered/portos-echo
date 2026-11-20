use super::*;
use serde_json::json;

fn id() -> TaskId {
    TaskId::new("task-1").unwrap()
}
fn session() -> Session {
    Session::new()
}
fn failed(message: &str) -> TaskEvent {
    TaskEvent::Failed {
        task_id: id(),
        retryable: false,
        message: message.into(),
    }
}

#[test]
fn muse_admission_requires_exact_created_shape_and_safe_identity() {
    assert_eq!(admission(201, br#"{"task_id":"task-1"}"#).unwrap(), id());
    for (status, body) in [
        (200, br#"{"task_id":"task-1"}"#.as_slice()),
        (204, b""),
        (201, br#"{"task_id":"task-1","other":1}"#),
        (201, br#"{"task_id":""}"#),
        (201, br#"{"task_id":"task/1"}"#),
        (201, br#"{"task_id":".."}"#),
        (201, br#"{"task_id":"task?query"}"#),
        (201, br#"{"task_id":4}"#),
        (201, b"[]"),
        (201, b"null"),
    ] {
        assert_eq!(admission(status, body), Err(Error::Decode));
    }
    assert_eq!(
        admission(201, json!({"task_id":"x".repeat(257)}).to_string().as_bytes()),
        Err(Error::Decode)
    );
}

#[tokio::test(start_paused = true)]
async fn muse_progress_does_not_complete_and_terminal_values_are_preserved() {
    for (kind, payload, expected) in [
        (
            "completed",
            json!({"summary":"result ☃"}),
            TaskEvent::Completed {
                task_id: id(),
                summary: "result ☃".into(),
            },
        ),
        (
            "failed",
            json!({"retryable":false,"message":"Muse test job failed"}),
            failed("Muse test job failed"),
        ),
    ] {
        let mut session = session();
        assert_eq!(session.before_poll(&id()), None);
        let progress = br#"{"type":"progress","task_id":"task-1","message":"Done!"}"#;
        assert_eq!(
            session.event(&id(), progress),
            TaskEvent::Progress {
                task_id: id(),
                message: "Done!".into()
            }
        );
        assert!(!session.terminal);
        assert!(!session.pending);
        let mut wire = payload;
        wire["type"] = kind.into();
        wire["task_id"] = "task-1".into();
        assert_eq!(session.before_poll(&id()), None);
        assert_eq!(session.event(&id(), wire.to_string().as_bytes()), expected);
        assert!(session.terminal);
        assert!(!session.pending);
    }
}

#[tokio::test(start_paused = true)]
async fn muse_wrong_identity_or_shape_never_produces_success_or_provider_diagnostics() {
    for wire in [
        br#"{"type":"completed","task_id":"other","summary":"secret"}"#.as_slice(),
        br#"{"type":"completed","task_id":"task-1"}"#,
        b"not-json",
        br#"{"type":"failed","task_id":"task-1","message":"secret"}"#,
    ] {
        let mut session = session();
        assert_eq!(session.event(&id(), wire), failed(UNAVAILABLE));
        assert!(session.terminal);
    }
}

#[tokio::test(start_paused = true)]
async fn muse_interrupted_dequeue_is_never_automatically_replayed() {
    let mut session = session();
    assert_eq!(session.before_poll(&id()), None);
    assert!(session.pending);
    assert_eq!(session.before_poll(&id()), Some(failed(UNAVAILABLE)));
    assert!(session.terminal);
}
