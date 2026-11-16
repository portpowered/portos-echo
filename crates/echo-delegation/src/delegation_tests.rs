use super::*;
use echo_agent_api::fakes::{BackendScript, FakeBackend};
use echo_contracts::ids::TaskId;
use serde_json::json;
use std::sync::Mutex;

#[derive(Default)]
struct Sink(Mutex<Vec<TraceEvent>>);
impl TraceSink for Sink {
    fn record(&self, event: TraceEvent) {
        self.0.lock().unwrap().push(event);
    }
}
fn config() -> AgentConfig {
    AgentConfig {
        model: Some("fixture-model".into()),
        max_tool_calls: 2,
        task_deadline_seconds: 7,
        daily_request_cap: 3,
    }
}
fn event() -> LiveEvent {
    serde_json::from_value(json!({"type":"session.delegation.created","delegation":{"id":"d-unit","target":"client","opaque":{"x":[1,"雪"]}},"extra":true})).unwrap()
}
#[test]
fn mapping_preserves_entire_event_and_config() {
    assert_eq!(
        task(&event(), "instructions".into(), &config()).unwrap(),
        Some(Task {
            delegation_id: DelegationId::new("d-unit").unwrap(),
            instructions: "instructions".into(),
            context: serde_json::to_value(event()).unwrap(),
            deadline_seconds: 7,
            max_tool_calls: 2,
        })
    );
    assert_eq!(
        TraceEvent::ToolCall {
            name: "test.echo".into()
        }
        .name(),
        "tools/call"
    );
}
#[tokio::test]
async fn refuses_target_and_ignores_unrelated_without_observation() {
    let backend = FakeBackend::default();
    let sink = Sink::default();
    let mut input = serde_json::to_value(event()).unwrap();
    input["delegation"]["target"] = json!("responses");
    assert_eq!(
        execute(
            &backend,
            &config(),
            &sink,
            serde_json::from_value(input).unwrap(),
            String::new()
        )
        .await,
        Err(DelegationError::UnsupportedTarget)
    );
    for input in [
        LiveEvent::Other(json!({"type":"unknown"})),
        LiveEvent::SessionCreated {
            extra: std::collections::BTreeMap::default(),
        },
    ] {
        assert_eq!(
            execute(&backend, &config(), &sink, input, String::new()).await,
            Ok(None)
        );
    }
    assert_eq!(backend.calls(), vec![]);
    assert_eq!(*sink.0.lock().unwrap(), vec![]);
}
#[tokio::test]
async fn terminal_text_submission_and_missing_terminal_are_exact() {
    let id = TaskId::new("task-unit").unwrap();
    for terminal in [
        Some(TaskEvent::Completed {
            task_id: id.clone(),
            summary: "雪☃".into(),
        }),
        Some(TaskEvent::Failed {
            task_id: id.clone(),
            retryable: false,
            message: "honest failure".into(),
        }),
        None,
    ] {
        let mut events = vec![TaskEvent::Progress {
            task_id: id.clone(),
            message: "working".into(),
        }];
        let expected = terminal.as_ref().map(|e| match e {
            TaskEvent::Completed { summary, .. } => summary.clone(),
            TaskEvent::Failed { message, .. } => message.clone(),
            TaskEvent::Progress { .. } => unreachable!(),
        });
        events.extend(terminal);
        let backend = FakeBackend::new(vec![BackendScript {
            task: task(&event(), "i".into(), &config()).unwrap().unwrap(),
            id: id.clone(),
            events,
        }]);
        let sink = Sink::default();
        let result = execute(&backend, &config(), &sink, event(), "i".into()).await;
        match expected {
            Some(text) => {
                assert_eq!(
                    result,
                    Ok(Some(LiveEvent::CommentaryAppend {
                        delegation_id: DelegationId::new("d-unit").unwrap(),
                        text,
                        extra: std::collections::BTreeMap::default()
                    }))
                );
                assert_eq!(sink.0.lock().unwrap()[1].name(), "session.commentary.append");
            }
            None => assert_eq!(result, Err(DelegationError::MissingTerminal)),
        }
        assert_eq!(sink.0.lock().unwrap()[0].name(), "session.delegation.created");
    }
    assert_eq!(
        execute(
            &FakeBackend::default(),
            &config(),
            &Sink::default(),
            event(),
            String::new()
        )
        .await,
        Err(DelegationError::Submission)
    );
}
