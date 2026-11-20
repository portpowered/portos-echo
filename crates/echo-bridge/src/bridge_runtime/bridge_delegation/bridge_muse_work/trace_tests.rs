//! T1: retained owner, in-memory backend/Live and per-case paused time/private trace.
use super::*;
use crate::bridge_runtime::bridge_delegation::bridge_muse_host::Muse;

fn rows(witness: &Witness, event: &str) -> Vec<Value> {
    witness
        .records()
        .into_iter()
        .filter(|row| row["event"] == event)
        .collect()
}

fn resolved(witness: &Witness, outcome: &str, handback: &str, answer: Option<&str>) -> Value {
    let created = rows(witness, "session.delegation.created");
    let terminal = rows(witness, "delegation.resolved");
    assert_eq!(created.len(), 1);
    assert_eq!(terminal.len(), 1);
    let row = terminal[0].clone();
    assert_eq!(row["delegation_id"], "delegation-raw-7");
    assert_eq!(row["run_id"], "run-7");
    assert_eq!(row["outcome"], outcome);
    assert_eq!(row["handback"], handback);
    assert_eq!(row["tool_counts"], json!({}));
    assert_eq!(row.get("answer").cloned(), answer.map(|text| json!({"text":text})));
    assert!(row.get("thread_id").is_none());
    assert!(row.get("turn_id").is_none());
    for row in [created[0].clone(), row.clone()] {
        println!("{row}");
    }
    row
}

fn events(case: &str) -> Vec<TaskEvent> {
    match case {
        "completed" => vec![TaskEvent::Completed {
            task_id: id(),
            summary: "synthetic result".into(),
        }],
        "failed" => vec![TaskEvent::Failed {
            task_id: id(),
            retryable: true,
            message: "untrusted provider message".into(),
        }],
        "malformed" => vec![TaskEvent::Completed {
            task_id: TaskId::new("foreign-task").unwrap(),
            summary: "foreign answer".into(),
        }],
        _ => vec![],
    }
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_muse_task_outcomes_keep_exact_speech_and_one_local_handback() {
    for case in ["completed", "failed", "rejected", "deadline", "malformed"] {
        let mut fake = fake(events(case));
        fake.reject = case == "rejected";
        let state = fake.state.clone();
        let mut host = Muse::new(fake, config(), Arc::new(|| 7));
        let mut witness = Witness::new();
        let event = delegation("delegation-raw-7", "client");
        assert!(host.dispatch(&event, "run-7", &mut witness.trace).unwrap());
        host.ready().await;
        let mut live = Live::default();
        let result = host.service(&mut live, &mut witness.trace).await;
        let (outcome, text, reason) = match case {
            "completed" => ("completed", Some("synthetic result"), None),
            "failed" => ("failed", Some(UNAVAILABLE), Some("task_failed")),
            "rejected" => ("failed", Some(UNAVAILABLE), Some("submission_failed")),
            "deadline" => ("timed_out", Some(DEADLINE), None),
            _ => ("failed", None, Some("worker_failed")),
        };
        assert_eq!(
            result,
            if case == "malformed" {
                Err(Error::Decode)
            } else {
                Ok(())
            }
        );
        assert_eq!(
            live.0,
            text.map(|text| vec![(task().delegation_id, text.into())])
                .unwrap_or_default()
        );
        host.service(&mut live, &mut witness.trace).await.unwrap();
        assert!(!host.abandon().await);
        drop(host);
        let row = resolved(
            &witness,
            outcome,
            if text.is_some() { "submitted" } else { "unobserved" },
            text,
        );
        assert_eq!(row.get("reason_class").and_then(Value::as_str), reason);
        assert_eq!(row["duration_ms"], if case == "deadline" { 5000 } else { 0 });
        assert_eq!(
            rows(&witness, "session.delegation.created")[0]["task"],
            serde_json::to_value(event).unwrap()
        );
        assert_eq!(state.tasks.lock().unwrap().len(), 1);
        assert_eq!(state.cancels.lock().unwrap().len(), usize::from(case == "malformed"));
    }
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_muse_append_failure_preserves_error_and_prior_task_failure() {
    for failed in [false, true] {
        let event = if failed {
            TaskEvent::Failed {
                task_id: id(),
                retryable: false,
                message: "Muse test job failed".into(),
            }
        } else {
            TaskEvent::Completed {
                task_id: id(),
                summary: "synthetic answer".into(),
            }
        };
        let mut host = Muse::new(fake(vec![event]), config(), Arc::new(|| 7));
        let mut witness = Witness::new();
        host.dispatch(&delegation("delegation-raw-7", "client"), "run-7", &mut witness.trace)
            .unwrap();
        host.ready().await;
        let mut live = Live(vec![], true);
        assert_eq!(host.service(&mut live, &mut witness.trace).await, Err(Error::Decode));
        host.service(&mut live, &mut witness.trace).await.unwrap();
        drop(host);
        assert_eq!(live.0.len(), 1);
        assert_eq!(rows(&witness, "session.commentary.append").len(), 0);
        let row = resolved(
            &witness,
            "failed",
            "failed",
            Some(if failed {
                "Muse test job failed"
            } else {
                "synthetic answer"
            }),
        );
        assert_eq!(
            row["reason_class"],
            if failed { "task_failed" } else { "append_failed" }
        );
    }
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_muse_abandon_drop_and_replacement_close_each_generation_once() {
    for explicit in [false, true] {
        let fake = fake(vec![]);
        let state = fake.state.clone();
        let mut agent = config();
        agent.daily_request_cap = 3;
        let mut host = Muse::new(fake, agent, Arc::new(|| 7));
        let mut witness = Witness::new();
        let event = delegation("delegation-raw-7", "client");
        host.dispatch(&event, "run-7", &mut witness.trace).unwrap();
        state.polled.notified().await;
        tokio::time::advance(Duration::from_millis(17)).await;
        if explicit {
            assert!(host.abandon().await);
            assert!(!host.abandon().await);
            host.new_call();
            host.dispatch(&event, "run-8", &mut witness.trace).unwrap();
            state.polled.notified().await;
            assert!(host.abandon().await);
            assert_eq!(*state.cancels.lock().unwrap(), vec![id(), id()]);
        }
        drop(host);
        let terminal = rows(&witness, "delegation.resolved");
        assert_eq!(terminal.len(), if explicit { 2 } else { 1 });
        assert_eq!(rows(&witness, "session.delegation.created").len(), terminal.len());
        assert_eq!(terminal[0]["duration_ms"], 17);
        for (index, row) in terminal.iter().enumerate() {
            assert_eq!(row["run_id"], if index == 0 { "run-7" } else { "run-8" });
            assert_eq!(row["outcome"], "cancelled");
            assert_eq!(row["handback"], "unobserved");
            assert!(row.get("answer").is_none());
            println!("{row}");
        }
        assert_eq!(rows(&witness, "session.commentary.append").len(), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn delegation_trace_muse_payload_switch_redacts_task_and_answer_without_changing_speech() {
    for payloads in [false, true] {
        let answer = r#"{"text":"synthetic-result-7","account_id":"synthetic-account-7"}"#;
        let fake = fake(vec![TaskEvent::Completed {
            task_id: id(),
            summary: answer.into(),
        }]);
        let mut host = Muse::new(fake, config(), Arc::new(|| 7));
        let mut witness = Witness::new();
        witness.trace = witness.trace.with_payloads(payloads);
        let mut input = serde_json::to_value(delegation("delegation-raw-7", "client")).unwrap();
        input["delegation"]["input"] = json!({"query":"synthetic-query-7", "url":"http://127.0.0.1/synthetic-source", "authorization":"Bearer synthetic-credential-7", "installation_id":"synthetic-installation-7"});
        host.dispatch(
            &serde_json::from_value(input.clone()).unwrap(),
            "run-7",
            &mut witness.trace,
        )
        .unwrap();
        host.ready().await;
        let mut live = Live::default();
        host.service(&mut live, &mut witness.trace).await.unwrap();
        drop(host);
        assert_eq!(live.0, vec![(task().delegation_id, answer.into())]);
        check_payloads(&witness, input, payloads);
    }
}

fn check_payloads(witness: &Witness, mut input: Value, payloads: bool) {
    let raw = std::fs::read_to_string(&witness.path).unwrap();
    for secret in [
        "synthetic-account-7",
        "synthetic-credential-7",
        "synthetic-installation-7",
    ] {
        assert!(!raw.contains(secret));
    }
    let created = rows(witness, "session.delegation.created");
    let terminal = rows(witness, "delegation.resolved");
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0]["handback"], "submitted");
    if payloads {
        input["delegation"]["input"]["authorization"] = json!("[redacted]");
        input["delegation"]["input"]["installation_id"] = json!("[redacted]");
        assert_eq!(created[0]["task"], input);
        assert_eq!(
            terminal[0]["answer"],
            json!({"text":r#"{"account_id":"[redacted]","text":"synthetic-result-7"}"#})
        );
    } else {
        for payload in [
            "synthetic-query-7",
            "synthetic-result-7",
            "http://127.0.0.1/synthetic-source",
            "private request",
        ] {
            assert!(!raw.contains(payload));
        }
        for row in &witness.records() {
            for key in ["task", "request", "response", "answer"] {
                assert!(row.get(key).is_none());
            }
        }
    }
    for row in [created[0].clone(), terminal[0].clone()] {
        println!("{row}");
    }
}
