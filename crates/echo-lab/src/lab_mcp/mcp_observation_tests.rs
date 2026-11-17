use super::*;
use echo_spoof_mcp::{CallRecord, RequestRecord};

fn lifecycle(observer: &mut Observations, name: &str, id: u64) {
    observer.trace(&json!({"event":name,"operation_id":id})).unwrap();
}

fn linked() -> Observations {
    let mut observer = Observations::default();
    lifecycle(&mut observer, "mcp.link.started", 1);
    lifecycle(&mut observer, "mcp.link.approved", 1);
    observer
}

fn output(result: &Value) -> Value {
    json!({"input":[{"type":"function_call_output","call_id":"call-1","output":result.to_string()}]})
}

fn success() -> Value {
    output(&json!({"content":[{"text":"m5-linked","type":"text"}],"is_error":false}))
}

fn resource() -> Snapshot {
    Snapshot {
        requests: ["tools/list", "tools/call"]
            .into_iter()
            .map(|method| RequestRecord {
                method: method.into(),
                id: Some(1),
                authorization_matches: Some(true),
                session_header: false,
                dpop_header: false,
            })
            .collect(),
        calls: vec![CallRecord {
            name: "test.echo".into(),
            args: json!({"text":"m5-linked"}),
            count: 1,
        }],
    }
}

#[test]
fn mcp_measurements_bind_all_eight_counts_to_independent_observations() {
    let mut observer = linked();
    lifecycle(&mut observer, "mcp.refresh.committed", 2);
    observer.restarted();
    lifecycle(&mut observer, "mcp.refresh.committed", 1);
    observer.trace(&json!({"event":"mcp.grant.reloaded"})).unwrap();
    observer.model("task-1", &success()).unwrap();
    let error = UiState::Error {
        message: RELINK.into(),
        relink: true,
    };
    observer.screen(&UiState::Listening);
    observer.screen(&error);
    observer.screen(&error);
    lifecycle(&mut observer, "mcp.unlink.completed", 3);
    let auth = Counts {
        links: 1,
        refreshes: 2,
        replay_rejected: 1,
        revokes: 1,
        polls: 1,
    };
    assert_eq!(
        observer.measure(auth, &resource()),
        Ok(LinkCounts {
            link: 1,
            tools_list: 1,
            tool_call: 1,
            forced_refresh: 2,
            replay_rejected: 1,
            relink_error: 1,
            restart_grant_kept: 1,
            unlink: 1,
        })
    );
    observer.model("task-1", &success()).unwrap();
    assert_eq!(observer.measure(auth, &resource()).unwrap().tool_call, 1);
}

#[test]
fn mcp_measurements_count_returned_successes_not_arrivals_or_rejected_attempts() {
    let mut observer = linked();
    let auth = Counts {
        links: 1,
        ..Counts::default()
    };
    let mut snapshot = resource();
    observer
        .model("failed-task", &output(&json!({"content":[],"is_error":true})))
        .unwrap();
    assert_eq!(observer.measure(auth, &snapshot).unwrap().tool_call, 0);
    snapshot.requests[0].authorization_matches = Some(false);
    snapshot.requests[1].authorization_matches = Some(false);
    snapshot.calls.clear();
    let counts = observer.measure(auth, &snapshot).unwrap();
    assert_eq!((counts.tools_list, counts.tool_call), (0, 0));
    observer.model("successful-task", &success()).unwrap();
    assert_eq!(observer.measure(auth, &snapshot), Err(INVALID));
}

#[test]
fn mcp_measurements_pending_denied_and_uncertain_operations_never_infer_completions() {
    let mut observer = Observations::default();
    lifecycle(&mut observer, "mcp.link.started", 1);
    let auth = Counts {
        links: 1,
        polls: 1,
        refreshes: 1,
        ..Counts::default()
    };
    assert_eq!(observer.measure(auth, &Snapshot::default()), Ok(LinkCounts::default()));
    observer
        .trace(&json!({"event":"mcp.relink_required","reason":"unauthorized"}))
        .unwrap();
    // A trace diagnostic alone does not prove that the screen displayed it.
    assert_eq!(observer.measure(auth, &Snapshot::default()).unwrap().relink_error, 0);
    observer.restarted();
    assert_eq!(observer.measure(auth, &Snapshot::default()), Ok(LinkCounts::default()));
}

#[test]
fn mcp_measurements_require_independent_signed_resource_and_fixture_agreement() {
    for field in ["links", "refreshes", "admission", "dpop", "call", "args"] {
        let mut observer = linked();
        observer.model("task-1", &success()).unwrap();
        let mut auth = Counts {
            links: 1,
            ..Counts::default()
        };
        let mut snapshot = resource();
        match field {
            "links" => auth.links = 0,
            "refreshes" => lifecycle(&mut observer, "mcp.refresh.committed", 2),
            "admission" => snapshot.requests[0].authorization_matches = None,
            "dpop" => snapshot.requests[0].dpop_header = true,
            "call" => snapshot.calls.clear(),
            "args" => snapshot.calls[0].args = json!({"text":"other"}),
            _ => unreachable!(),
        }
        assert_eq!(observer.measure(auth, &snapshot), Err(INVALID), "{field}");
    }
}

#[test]
fn mcp_measurements_reject_duplicate_malformed_and_uncorrelated_operations() {
    for event in [
        json!({"event":"mcp.link.approved","operation_id":2}),
        json!({"event":"mcp.refresh.committed","operation_id":1}),
        json!({"event":"mcp.unlink.completed","operation_id":null}),
        json!({"event":"mcp.link.started","operation_id":0}),
        json!({"event":"mcp.link.started","operation_id":1}),
    ] {
        let mut observer = linked();
        assert_eq!(observer.trace(&event), Err(INVALID));
        observer.restarted();
        assert_eq!(observer.measure(Counts::default(), &Snapshot::default()), Err(INVALID));
    }
    let mut observer = Observations::default();
    lifecycle(&mut observer, "mcp.link.started", 1);
    assert_eq!(
        observer.trace(&json!({"event":"mcp.refresh.committed","operation_id":1})),
        Err(INVALID)
    );
}

#[test]
fn mcp_measurements_restart_requires_successful_use_and_accepts_either_observer_order() {
    let auth = Counts {
        links: 1,
        ..Counts::default()
    };
    for trace_first in [true, false] {
        let mut observer = linked();
        observer.restarted();
        if trace_first {
            observer.trace(&json!({"event":"mcp.grant.reloaded"})).unwrap();
        }
        assert_eq!(observer.measure(auth, &resource()).unwrap().restart_grant_kept, 0);
        observer.model("task-1", &success()).unwrap();
        if !trace_first {
            observer.trace(&json!({"event":"mcp.grant.reloaded"})).unwrap();
        }
        observer.model("task-1", &success()).unwrap();
        assert_eq!(observer.measure(auth, &resource()).unwrap().restart_grant_kept, 1);
        assert_eq!(observer.trace(&json!({"event":"mcp.grant.reloaded"})), Err(INVALID));
    }
    assert_eq!(
        Observations::default().trace(&json!({"event":"mcp.grant.reloaded"})),
        Err(INVALID)
    );
}

#[test]
fn mcp_measurements_model_results_are_delegation_scoped_and_fail_closed() {
    let mut observer = linked();
    observer.model("task-1", &success()).unwrap();
    observer.model("task-1", &success()).unwrap();
    observer.model("task-2", &success()).unwrap();
    let mut snapshot = resource();
    snapshot.requests.push(snapshot.requests[1].clone());
    snapshot.calls.push(snapshot.calls[0].clone());
    assert_eq!(
        observer
            .measure(
                Counts {
                    links: 1,
                    ..Counts::default()
                },
                &snapshot
            )
            .unwrap()
            .tool_call,
        2
    );
    let changed = output(&json!({"content":[],"is_error":true}));
    assert_eq!(observer.model("task-1", &changed), Err(INVALID));
    assert_eq!(observer.measure(Counts::default(), &Snapshot::default()), Err(INVALID));
    for request in [
        json!({}),
        output(&json!({"is_error":false})),
        json!({"input":[{"type":"function_call_output","call_id":"x","output":"invalid"}]}),
        json!({"input":[{"type":"function_call_output","call_id":"","output":"{}"}]}),
    ] {
        assert_eq!(Observations::default().model("task", &request), Err(INVALID));
    }
}

#[test]
fn mcp_measurements_screen_requires_the_exact_mcp_instruction_and_new_transition() {
    let mut observer = Observations::default();
    observer.screen(&UiState::Error {
        message: "Sign in to ChatGPT".into(),
        relink: true,
    });
    observer.screen(&UiState::Error {
        message: RELINK.into(),
        relink: false,
    });
    let error = UiState::Error {
        message: RELINK.into(),
        relink: true,
    };
    observer.screen(&error);
    observer.screen(&error);
    assert_eq!(
        observer
            .measure(Counts::default(), &Snapshot::default())
            .unwrap()
            .relink_error,
        1
    );
    observer.screen(&UiState::Ready {
        message: "Ready".into(),
    });
    observer.screen(&error);
    assert_eq!(
        observer
            .measure(Counts::default(), &Snapshot::default())
            .unwrap()
            .relink_error,
        2
    );
}

#[test]
fn mcp_codex_model_counts_exact_native_content_once_and_preserves_error_identity() {
    let mut observer = linked();
    observer.restarted();
    observer.trace(&json!({"event":"mcp.grant.reloaded"})).unwrap();
    let request = output(&json!([{"text":"m5-linked","type":"text"}]));
    observer.codex_model("thread-1", &request).unwrap();
    observer.codex_model("thread-1", &request).unwrap();
    let failed = json!({"input":[{"type":"function_call_output","call_id":"call-2",
        "output":"tool authorization required"}]});
    observer.codex_model("thread-1", &failed).unwrap();
    observer.codex_model("thread-1", &failed).unwrap();
    let counts = observer
        .measure(
            Counts {
                links: 1,
                ..Counts::default()
            },
            &resource(),
        )
        .unwrap();
    assert_eq!((counts.tool_call, counts.restart_grant_kept), (1, 1));
    let changed = json!({"input":[{"type":"function_call_output","call_id":"call-2",
        "output":"tool execution failed"}]});
    assert_eq!(observer.codex_model("thread-1", &changed), Err(INVALID));
}

#[test]
fn mcp_codex_model_rejects_legacy_or_extra_content_and_unrecognized_plaintext() {
    for request in [
        success(),
        output(&json!({"is_error":true})),
        output(&json!("tool execution failed")),
        output(&json!("unproved success")),
        output(&json!([])),
        output(&json!([{"type":"text","text":"m5-linked"},{"type":"text","text":"extra"}])),
        json!({"input":[{"type":"function_call_output","call_id":"call-1","output":"unproved success"}]}),
    ] {
        assert_eq!(Observations::default().codex_model("thread-1", &request), Err(INVALID));
    }
}
