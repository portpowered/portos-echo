use super::*;
use echo_agent_api::{ToolResult as HostResult, ToolSpec};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default, Clone)]
struct Host {
    calls: Arc<Mutex<Vec<(String, Value, String)>>>,
    error: Option<ToolError>,
    pending: bool,
    is_error: bool,
}
impl ToolHost for Host {
    fn list_tools(&self) -> impl Future<Output = Result<Vec<ToolSpec>, ToolError>> + Send {
        std::future::ready(Ok(vec![]))
    }
    async fn call_tool(&self, name: &str, args: Value, key: &str) -> Result<HostResult, ToolError> {
        self.calls.lock().unwrap().push((name.into(), args.clone(), key.into()));
        if self.pending {
            return std::future::pending().await;
        }
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(HostResult {
            content: args,
            is_error: self.is_error,
        })
    }
}

fn catalog() -> Catalog {
    Catalog::new(
        vec![ToolSpec {
            name: "test.echo".into(),
            description: "Echo text".into(),
            input_schema: json!({"type":"object"}),
        }],
        &["test.echo".into()],
    )
    .unwrap()
}
fn dispatcher(host: Host, limit: u32) -> Dispatcher<Host> {
    let mut dispatcher = Dispatcher::new(
        host,
        catalog(),
        "thread-1".into(),
        AgentConfig {
            model: None,
            max_tool_calls: limit,
            task_deadline_seconds: 5,
            daily_request_cap: 1,
        },
    )
    .unwrap();
    dispatcher.start_turn("thread-1", "turn-1").unwrap();
    dispatcher
}
fn call(index: usize) -> ToolCall {
    ToolCall {
        thread_id: "thread-1".into(),
        turn_id: "turn-1".into(),
        call_id: format!("call-{index}"),
        tool: "tool_0".into(),
        arguments: json!({"text":"雪"}),
        namespace: None,
    }
}

#[test]
fn catalog_preserves_definitions_and_filters_authority_without_alias_ambiguity() {
    let specs = (0..4)
        .map(|index| ToolSpec {
            name: format!("test.{index}"),
            description: index.to_string(),
            input_schema: json!({"type":"object","required":["text"]}),
        })
        .collect::<Vec<_>>();
    let catalog = Catalog::new(specs.clone(), &["test.0".into(), "test.2".into()]).unwrap();
    assert_eq!(
        serde_json::to_value(catalog.tools()).unwrap(),
        json!([
            {"name":"tool_0","description":"0","inputSchema":specs[0].input_schema},
            {"name":"tool_1","description":"2","inputSchema":specs[2].input_schema}
        ])
    );
    assert_eq!(catalog.resolve("tool_0"), Some("test.0"));
    assert_eq!(catalog.resolve("tool_1"), Some("test.2"));
    for alias in [
        "test.0",
        "tool_00",
        "tool_+0",
        "tool_2",
        "tool_-1",
        "tool_99999999999999999999999999",
    ] {
        assert_eq!(catalog.resolve(alias), None);
    }
    assert_eq!(Catalog::new(specs.clone(), &[]).unwrap().tools(), []);
    let mut duplicate = specs;
    duplicate.push(duplicate[0].clone());
    assert!(matches!(Catalog::new(duplicate, &[]), Err(DispatchError::Catalog)));
}

#[tokio::test(start_paused = true)]
async fn dispatch_preserves_result_and_reserves_duplicate_ids_before_effects() {
    let host = Host::default();
    let mut dispatcher = dispatcher(host.clone(), 8);
    let first = call(0);
    let outcome = dispatcher.dispatch(&first).await;
    assert!(outcome.result.success);
    assert!(outcome.interrupt.is_none());
    assert_eq!(
        serde_json::to_value(outcome.result).unwrap(),
        json!({"contentItems":[{"type":"inputText","text":first.arguments.to_string()}],"success":true})
    );
    assert!(!dispatcher.dispatch(&first).await.result.success);
    assert_eq!(
        *host.calls.lock().unwrap(),
        vec![("test.echo".into(), first.arguments, "8:thread-16:turn-16:call-0".into())]
    );
    assert!(dispatcher.dispatch(&call(1)).await.result.success);
    assert_eq!(host.calls.lock().unwrap().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn foreign_unknown_namespace_and_stale_requests_have_no_effect() {
    let host = Host::default();
    let mut dispatcher = dispatcher(host.clone(), 8);
    let mut requests = vec![call(0), call(1), call(2), call(3), call(4)];
    requests[0].thread_id = "foreign".into();
    requests[1].turn_id = "old-turn".into();
    requests[2].tool = "test.echo".into();
    requests[3].namespace = Some("model-consent".into());
    requests[4].arguments = Value::Null;
    for request in requests {
        let outcome = dispatcher.dispatch(&request).await;
        assert!(!outcome.result.success);
        assert!(outcome.interrupt.is_none());
    }
    assert_eq!(*host.calls.lock().unwrap(), []);
    assert_eq!(dispatcher.finish_turn("thread-1", "foreign"), Err(DispatchError::Stale));
    dispatcher.finish_turn("thread-1", "turn-1").unwrap();
    assert!(!dispatcher.dispatch(&call(5)).await.result.success);
    assert_eq!(dispatcher.start_turn("thread-1", "turn-1"), Err(DispatchError::Stale));
    dispatcher.start_turn("thread-1", "turn-2").unwrap();
    assert!(!dispatcher.dispatch(&call(6)).await.result.success);
    assert_eq!(*host.calls.lock().unwrap(), []);
}

#[tokio::test(start_paused = true)]
async fn configured_and_hard_limits_interrupt_exact_turn_without_extra_effects() {
    for limit in [1, 8, 12] {
        let host = Host::default();
        let mut dispatcher = dispatcher(host.clone(), limit);
        let admitted = limit.min(8) as usize;
        for index in 0..admitted {
            assert!(dispatcher.dispatch(&call(index)).await.result.success);
        }
        let outcome = dispatcher.dispatch(&call(admitted)).await;
        assert!(!outcome.result.success);
        assert_eq!(
            serde_json::to_value(outcome.interrupt).unwrap(),
            json!({"threadId":"thread-1","turnId":"turn-1"})
        );
        assert_eq!(host.calls.lock().unwrap().len(), admitted);
        let terminal = dispatcher.dispatch(&call(admitted + 1)).await;
        assert!(!terminal.result.success);
        assert!(terminal.interrupt.is_none());
        assert_eq!(host.calls.lock().unwrap().len(), admitted);
    }
}

#[tokio::test(start_paused = true)]
async fn deadline_cancels_pending_work_and_never_admits_late_or_replayed_calls() {
    let host = Host {
        pending: true,
        ..Host::default()
    };
    let mut dispatcher = dispatcher(host.clone(), 8);
    let start = Instant::now();
    let outcome = dispatcher.dispatch(&call(0)).await;
    assert_eq!(start.elapsed(), Duration::from_secs(5));
    assert!(!outcome.result.success);
    assert_eq!(
        serde_json::to_value(outcome.interrupt).unwrap(),
        json!({"threadId":"thread-1","turnId":"turn-1"})
    );
    assert!(!dispatcher.dispatch(&call(1)).await.result.success);
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    dispatcher.finish_turn("thread-1", "turn-1").unwrap();
    dispatcher.start_turn("thread-1", "turn-2").unwrap();
    assert!(!dispatcher.dispatch(&call(0)).await.result.success);
    assert_eq!(host.calls.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn cancellation_and_error_results_cannot_replay_or_leak_remote_diagnostics() {
    let host = Host {
        pending: true,
        ..Host::default()
    };
    let mut owner = dispatcher(host.clone(), 8);
    let request = call(0);
    {
        let future = owner.dispatch(&request);
        tokio::pin!(future);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(future.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    assert!(!owner.dispatch(&request).await.result.success);
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    for (error, text) in [
        (ToolError::Denied, "tool request denied"),
        (ToolError::Unauthorized, "tool authorization required"),
        (ToolError::Timeout, "tool deadline exceeded"),
        (
            ToolError::Server("private remote payload".into()),
            "tool execution failed",
        ),
        (ToolError::Transport, "tool execution failed"),
    ] {
        let mut owner = dispatcher(
            Host {
                error: Some(error),
                ..Host::default()
            },
            8,
        );
        let outcome = owner.dispatch(&call(0)).await;
        assert_eq!(
            serde_json::to_value(outcome.result).unwrap(),
            json!({"contentItems":[{"type":"inputText","text":text}],"success":false})
        );
        assert!(outcome.interrupt.is_none());
    }
}

#[test]
fn daily_budget_resets_only_on_forward_day_and_retains_cap_across_calls() {
    let mut budget = DailyBudget::new(1);
    assert_eq!(budget.admit(10), Ok(()));
    assert_eq!(budget.admit(10), Err(DispatchError::Budget));
    assert_eq!(budget.admit(9), Err(DispatchError::Budget));
    assert_eq!(budget.admit(11), Ok(()));
    assert_eq!(budget.admit(11), Err(DispatchError::Budget));
    assert_eq!(DailyBudget::new(0).admit(10), Err(DispatchError::Budget));
}

#[tokio::test(start_paused = true)]
async fn silent_turn_expires_at_equality_once_and_cannot_clear_a_new_turn() {
    let host = Host::default();
    let mut owner = dispatcher(host.clone(), 8);
    assert!(owner.expire().is_none());
    let deadline = owner.deadline().unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(Instant::now(), deadline);
    assert_eq!(
        serde_json::to_value(owner.expire()).unwrap(),
        json!({"threadId":"thread-1","turnId":"turn-1"})
    );
    assert!(owner.deadline().is_none());
    assert!(owner.expire().is_none());
    assert!(!owner.dispatch(&call(0)).await.result.success);
    assert_eq!(*host.calls.lock().unwrap(), []);
    owner.finish_turn("thread-1", "turn-1").unwrap();
    owner.start_turn("thread-1", "turn-2").unwrap();
    assert_eq!(owner.finish_turn("thread-1", "turn-1"), Err(DispatchError::Stale));
    assert_eq!(owner.deadline(), Some(deadline + Duration::from_secs(5)));
}

#[tokio::test(start_paused = true)]
async fn invalid_configuration_and_turn_admission_cannot_replace_live_work() {
    let config = AgentConfig {
        model: None,
        max_tool_calls: 8,
        task_deadline_seconds: 5,
        daily_request_cap: 1,
    };
    for thread in ["", "bad\nthread"] {
        assert!(matches!(
            Dispatcher::new(Host::default(), catalog(), thread.into(), config.clone()),
            Err(DispatchError::Configuration)
        ));
    }
    for (limit, seconds) in [(0, 5), (1, 0), (1, u64::MAX)] {
        let config = AgentConfig {
            max_tool_calls: limit,
            task_deadline_seconds: seconds,
            ..config.clone()
        };
        assert!(matches!(
            Dispatcher::new(Host::default(), catalog(), "thread-1".into(), config),
            Err(DispatchError::Configuration)
        ));
    }
    let mut owner = dispatcher(Host::default(), 8);
    assert_eq!(owner.start_turn("thread-1", "turn-2"), Err(DispatchError::Stale));
    owner.finish_turn("thread-1", "turn-1").unwrap();
    for (thread, turn) in [("foreign", "turn-2"), ("thread-1", ""), ("thread-1", "bad\nturn")] {
        assert_eq!(owner.start_turn(thread, turn), Err(DispatchError::Stale));
    }
    owner.start_turn("thread-1", "turn-2").unwrap();
    let mut request = call(0);
    request.turn_id = "turn-2".into();
    assert!(owner.dispatch(&request).await.result.success);
}

#[tokio::test(start_paused = true)]
async fn deadline_before_dispatch_and_mcp_error_content_never_claim_success() {
    let host = Host::default();
    let mut owner = dispatcher(host.clone(), 8);
    tokio::time::advance(Duration::from_secs(5)).await;
    let outcome = owner.dispatch(&call(0)).await;
    assert!(!outcome.result.success);
    assert_eq!(
        serde_json::to_value(outcome.interrupt).unwrap(),
        json!({"threadId":"thread-1","turnId":"turn-1"})
    );
    assert_eq!(*host.calls.lock().unwrap(), []);
    let mut owner = dispatcher(
        Host {
            is_error: true,
            ..Host::default()
        },
        8,
    );
    let outcome = owner.dispatch(&call(0)).await;
    assert_eq!(
        serde_json::to_value(outcome.result).unwrap(),
        json!({"contentItems":[{"type":"inputText","text":"{\"text\":\"雪\"}"}],"success":false})
    );
    assert!(outcome.interrupt.is_none());
}

#[test]
fn malformed_or_oversized_catalog_is_not_advertised() {
    let spec = ToolSpec {
        name: "test.echo".into(),
        description: "Echo".into(),
        input_schema: json!({"type":"object"}),
    };
    assert!(matches!(
        Catalog::new(vec![spec.clone(); 1025], &[]),
        Err(DispatchError::Catalog)
    ));
    for malformed in [
        ToolSpec {
            name: String::new(),
            ..spec.clone()
        },
        ToolSpec {
            input_schema: Value::Null,
            ..spec
        },
    ] {
        assert!(matches!(
            Catalog::new(vec![malformed], &[]),
            Err(DispatchError::Catalog)
        ));
    }
}
