use crate::fakes::{BackendCall, BackendScript, FakeBackend, FakeToolHost, ToolCall, ToolScript};
use crate::*;
use pollster::block_on;
use serde_json::json;

fn catalog() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "test.echo".into(),
        description: "Echo a message".into(),
        input_schema: json!({"type": "object", "properties": {"message": {"type": "string"}}}),
    }]
}

fn call() -> ToolCall {
    ToolCall {
        name: "test.echo".into(),
        args: json!({"message": "hello", "nested": [1, null, true]}),
        idempotency_key: "key-1".into(),
    }
}

fn result() -> ToolResult {
    ToolResult {
        content: json!({"echo": "hello"}),
        is_error: false,
    }
}

fn script() -> ToolScript {
    ToolScript {
        call: call(),
        result: Ok(result()),
    }
}

fn invoke(host: &impl ToolHost, request: &ToolCall) -> Result<ToolResult, ToolError> {
    block_on(host.call_tool(&request.name, request.args.clone(), &request.idempotency_key))
}

#[test]
fn tool_t1_catalog_and_exact_success() {
    let host = FakeToolHost::new(Ok(catalog()), vec![script()]);
    assert_eq!(block_on(host.list_tools()), Ok(catalog()));
    assert_eq!(invoke(&host, &call()), Ok(result()));
    assert_eq!(host.calls(), vec![call()]);
    // Mutating a returned catalog or recording cannot change the host.
    let mut snapshot = host.calls();
    snapshot[0].name = "changed".into();
    let mut tools = block_on(host.list_tools()).unwrap();
    tools.clear();
    assert_eq!(block_on(host.list_tools()), Ok(catalog()));
    assert_eq!(host.calls(), vec![call()]);
}

#[test]
fn tool_t2_empty_catalog_and_error_result_is_success() {
    let expected = ToolResult {
        content: json!({"error": ["invalid"]}),
        is_error: true,
    };
    let host = FakeToolHost::new(
        Ok(vec![]),
        vec![ToolScript {
            result: Ok(expected.clone()),
            ..script()
        }],
    );
    assert_eq!(block_on(host.list_tools()), Ok(vec![]));
    assert_eq!(invoke(&host, &call()), Ok(expected));
    assert_eq!(host.calls(), vec![call()]);
}

#[test]
fn tool_t3_exact_failures_without_retry() {
    let errors = [
        ToolError::Transport,
        ToolError::Unauthorized,
        ToolError::Denied,
        ToolError::Timeout,
        ToolError::Server("detail".into()),
    ];
    let displays = [
        "transport error",
        "unauthorized",
        "denied",
        "timeout",
        "server error: detail",
    ];
    for (error, display) in errors.into_iter().zip(displays) {
        let host = FakeToolHost::new(
            Err(error.clone()),
            vec![ToolScript {
                result: Err(error.clone()),
                ..script()
            }],
        );
        assert_eq!(block_on(host.list_tools()), Err(error.clone()));
        assert_eq!(block_on(host.list_tools()), Err(error.clone()));
        assert_eq!(invoke(&host, &call()), Err(error.clone()));
        assert_eq!(host.calls(), vec![call()]);
        assert_eq!(error.to_string(), display);
        assert_eq!(
            invoke(&host, &call()),
            Err(ToolError::Server("unscripted tool call".into()))
        );
        assert_eq!(host.calls(), vec![call(), call()]);
    }
}

#[test]
fn tool_t4_mismatches_preserve_unrelated_scripts() {
    let mut requests = vec![call(); 3];
    requests[0].name = "test.other".into();
    requests[1].args["nested"][0] = json!(2);
    requests[2].idempotency_key = "key-2".into();
    let mut later = script();
    later.call.name = "test.later".into();
    later.result = Ok(ToolResult {
        content: json!(["later"]),
        is_error: false,
    });
    let host = FakeToolHost::new(Ok(catalog()), vec![script(), later.clone()]);
    for request in &requests {
        assert_eq!(
            invoke(&host, request),
            Err(ToolError::Server("unscripted tool call".into()))
        );
    }
    assert_eq!(host.calls(), requests);
    assert_eq!(invoke(&host, &later.call), later.result);
    assert_eq!(invoke(&host, &call()), Ok(result()));
    requests.extend([later.call, call()]);
    assert_eq!(host.calls(), requests);
}

#[test]
fn tool_t5_duplicate_keys_consume_scripts_in_order() {
    let second = ToolResult {
        content: json!([2, {"opaque": null}]),
        is_error: true,
    };
    let host = FakeToolHost::new(
        Ok(vec![]),
        vec![
            script(),
            ToolScript {
                result: Ok(second.clone()),
                ..script()
            },
        ],
    );
    assert_eq!(invoke(&host, &call()), Ok(result()));
    assert_eq!(invoke(&host, &call()), Ok(second));
    assert_eq!(host.calls(), vec![call(), call()]);
    assert_eq!(
        invoke(&host, &call()),
        Err(ToolError::Server("unscripted tool call".into()))
    );
    assert_eq!(host.calls(), vec![call(), call(), call()]);
}

#[test]
fn tool_t6_instances_isolate_scripts_and_recordings() {
    let mut other = script();
    other.call.idempotency_key = "other-key".into();
    other.result = Err(ToolError::Denied);
    let first = FakeToolHost::new(Ok(catalog()), vec![script()]);
    let second = FakeToolHost::new(Ok(vec![]), vec![other.clone()]);
    assert_eq!(
        invoke(&first, &other.call),
        Err(ToolError::Server("unscripted tool call".into()))
    );
    assert_eq!(invoke(&second, &other.call), Err(ToolError::Denied));
    assert_eq!(invoke(&first, &call()), Ok(result()));
    assert_eq!(
        invoke(&second, &call()),
        Err(ToolError::Server("unscripted tool call".into()))
    );
    assert_eq!(first.calls(), vec![other.call.clone(), call()]);
    assert_eq!(second.calls(), vec![other.call, call()]);
}

fn task() -> Task {
    Task {
        delegation_id: DelegationId::new("d-1").unwrap(),
        instructions: "echo".into(),
        context: json!({"message": "hello"}),
        deadline_seconds: 60,
        max_tool_calls: 8,
    }
}

fn backend() -> FakeBackend {
    let id = TaskId::new("task-1").unwrap();
    FakeBackend::new(vec![BackendScript {
        task: task(),
        id: id.clone(),
        events: vec![
            TaskEvent::Progress {
                task_id: id.clone(),
                message: "working".into(),
            },
            TaskEvent::Completed {
                task_id: id,
                summary: "done".into(),
            },
        ],
    }])
}

fn assert_send<T: Send>(value: T) -> T {
    value
}

fn send_backend(backend: &impl AgentBackend) {
    let id = block_on(assert_send(backend.submit(task()))).unwrap();
    assert_eq!(id, TaskId::new("task-1").unwrap());
    assert_eq!(
        block_on(assert_send(backend.next_event(&id))),
        Some(TaskEvent::Progress {
            task_id: id.clone(),
            message: "working".into(),
        })
    );
    assert_eq!(block_on(assert_send(backend.cancel(&id))), Ok(()));
    assert_eq!(block_on(backend.next_event(&id)), None);
}

fn send_tool(host: &impl ToolHost) {
    assert_eq!(block_on(assert_send(host.list_tools())), Ok(catalog()));
    let request = call();
    assert_eq!(
        block_on(assert_send(host.call_tool(
            &request.name,
            request.args,
            &request.idempotency_key
        ))),
        Ok(result())
    );
}

#[test]
fn send_s1_generic_promises_and_concrete_fake_future() {
    send_backend(&backend());
    let host = FakeToolHost::new(Ok(catalog()), vec![script()]);
    send_tool(&host);
    assert_eq!(block_on(assert_send(host.list_tools())), Ok(catalog()));
    assert_eq!(host.calls(), vec![call()]);
}

fn journey(backend: &impl AgentBackend, host: &impl ToolHost) {
    let id = block_on(backend.submit(task())).unwrap();
    assert_eq!(id, TaskId::new("task-1").unwrap());
    assert_eq!(block_on(host.list_tools()), Ok(catalog()));
    assert_eq!(invoke(host, &call()), Ok(result()));
    assert_eq!(
        block_on(backend.next_event(&id)),
        Some(TaskEvent::Progress {
            task_id: id.clone(),
            message: "working".into(),
        })
    );
    assert_eq!(
        block_on(backend.next_event(&id)),
        Some(TaskEvent::Completed {
            task_id: id.clone(),
            summary: "done".into(),
        })
    );
    assert_eq!(block_on(backend.next_event(&id)), None);
}

#[test]
fn assembled_j1_submit_call_and_scripted_completion() {
    let backend = backend();
    let host = FakeToolHost::new(Ok(catalog()), vec![script()]);
    journey(&backend, &host);
    assert_eq!(host.calls(), vec![call()]);
    let id = TaskId::new("task-1").unwrap();
    assert_eq!(
        backend.calls(),
        vec![
            BackendCall::Submit(task()),
            BackendCall::NextEvent(id.clone()),
            BackendCall::NextEvent(id.clone()),
            BackendCall::NextEvent(id)
        ]
    );
}
