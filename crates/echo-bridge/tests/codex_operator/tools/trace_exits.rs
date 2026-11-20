//! T3: actual configured owner exits, controlled child/media, real dispatcher/MCP.
use super::*;

#[tokio::test]
#[ignore = "T3 controlled child and loopback MCP"]
async fn delegation_trace_configured_native_managed_exits_close_once_without_effects() {
    for cause in [
        "child",
        "failed",
        "error",
        "closed",
        "malformed",
        "empty",
        "shutdown",
        "media",
        "kill",
    ] {
        let fixture = Fixture::new("chatgpt");
        journey(&fixture, cause).await;
    }
}

#[tokio::test]
#[ignore = "T3 controlled child and loopback MCP"]
async fn delegation_trace_configured_native_deadline_and_actual_interrupt_keep_first_outcome() {
    for cause in ["timeout", "interrupt"] {
        let fixture = Fixture::new("chatgpt");
        journey(&fixture, cause).await;
    }
}

async fn journey(fixture: &Fixture, cause: &str) {
    std::fs::write(fixture.home.join("mode"), format!("tool-trace-exit-{cause}")).unwrap();
    let spoof = echo_spoof_mcp::Spoof::start(echo_spoof_mcp::Options::default())
        .await
        .unwrap();
    let mut config = config(fixture, spoof.url());
    if cause == "interrupt" {
        config.agent.max_tool_calls = 1;
    }
    if cause == "timeout" {
        config.agent.task_deadline_seconds = 1;
    }
    let observer = Observer::default();
    let closed = Arc::new(AtomicUsize::new(0));
    let lost = Arc::new(tokio::sync::Notify::new());
    let prepared = Configured::new(
        config,
        fixture.directory.path().join("tool-trace"),
        ([127, 0, 0, 1], 0).into(),
    )
    .unwrap();
    let (stop, stopping) = tokio::sync::oneshot::channel();
    let work = prepared.execute(
        FakeConnector::new(observer.clone()),
        async || {
            let mut peer = media(&closed);
            peer.lost = lost.clone();
            Ok(peer)
        },
        vec![],
        async {
            stopping.await.unwrap();
        },
    );
    let inspect = inspect(fixture, cause, &observer, &lost, stop);
    let (outcome, ()) = Box::pin(tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        tokio::join!(work, inspect)
    }))
    .await
    .unwrap();
    assert_eq!(outcome, Ok(()));
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(observer.commands().last(), Some(&echo_link::fake::Command::Close));
    assert_rows(fixture, cause);
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 0);
    super::super::host::released(fixture).await;
    spoof.shutdown().await.unwrap();
}

async fn inspect(
    fixture: &Fixture,
    cause: &str,
    observer: &Observer,
    lost: &tokio::sync::Notify,
    stop: tokio::sync::oneshot::Sender<()>,
) {
    let (mut control, mut screen) = ready(fixture).await;
    observer.wake("native trace exit", vec![]);
    wait(fixture, "delegation.created").await;
    control.write_all(b"exit").await.unwrap();
    let mut stop = Some(stop);
    match cause {
        "shutdown" => stop.take().unwrap().send(()).unwrap(),
        "media" => lost.notify_one(),
        "kill" => std::fs::write(fixture.directory.path().join("audit/bridge-kill"), []).unwrap(),
        "timeout" => {
            while bridge_wire_screen::state(&mut screen).await["state"] != "approval" {}
            let clock = bridge_short::ReadyClock::new();
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(1)).await;
            wait(fixture, "delegation.resolved").await;
            tokio::time::resume();
            drop(clock);
        }
        _ => (),
    }
    wait(fixture, "delegation.resolved").await;
    if let Some(stop) = stop {
        stop.send(()).unwrap();
    }
    observer.finish(Ok(()));
}

async fn wait(fixture: &Fixture, event: &str) {
    loop {
        if trace(&fixture.directory.path().join("tool-trace"))
            .iter()
            .any(|row| row["event"] == event)
        {
            return;
        }
        tokio::task::yield_now().await;
    }
}

fn assert_rows(fixture: &Fixture, cause: &str) {
    let rows = trace(&fixture.directory.path().join("tool-trace"));
    let created: Vec<_> = rows.iter().filter(|row| row["event"] == "delegation.created").collect();
    let resolved: Vec<_> = rows
        .iter()
        .filter(|row| row["event"] == "delegation.resolved")
        .collect();
    assert_eq!(created.len(), 1);
    assert_eq!(resolved.len(), 1);
    writeln!(std::io::stdout(), "native exit {cause}: {}", resolved[0]).unwrap();
    assert_terminal(resolved[0], created[0], cause);
    assert_counts(&rows, resolved[0], cause);
    assert!(
        !std::fs::read_to_string(fixture.directory.path().join("tool-trace"))
            .unwrap()
            .contains("synthetic-private-error")
    );
    writeln!(std::io::stdout(), "{}", resolved[0]).unwrap();
}

fn expected(cause: &str) -> (&str, Option<&str>) {
    match cause {
        "child" => ("failed", Some("app_server_exit")),
        "failed" => ("failed", Some("turn_failed")),
        "error" => ("failed", Some("realtime_error")),
        "closed" => ("failed", Some("realtime_closed")),
        "malformed" => ("failed", Some("control_failed")),
        "media" => ("failed", Some("media_failed")),
        "empty" => ("completed", None),
        "timeout" => ("timed_out", None),
        "interrupt" => ("interrupted", None),
        _ => ("cancelled", None),
    }
}

fn assert_terminal(resolved: &Value, created: &Value, cause: &str) {
    let (outcome, reason) = expected(cause);
    assert_eq!(resolved["outcome"], outcome);
    assert_eq!(
        resolved.get("reason_class"),
        reason.map(|reason| json!(reason)).as_ref()
    );
    assert_eq!(resolved["handback"], "unobserved");
    assert_eq!(resolved["delegation_id"], created["delegation_id"]);
    assert_eq!(resolved["thread_id"], "fixture-thread");
    assert_eq!(resolved["run_id"], "run-1");
    assert!(resolved.get("answer").is_none());
}

fn assert_counts(rows: &[Value], resolved: &Value, cause: &str) {
    let tools: Vec<_> = rows.iter().filter(|row| row["event"] == "delegation.tool").collect();
    if cause == "interrupt" {
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["status"], "failed");
        assert_eq!(resolved["tool_counts"], json!({"mcp":1}));
        for event in ["turn/interrupt", "turn.interrupt_acknowledged", "turn.interrupted"] {
            let receipts: Vec<_> = rows.iter().filter(|row| row["event"] == event).collect();
            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0]["turn_id"], "fixture-turn");
        }
    } else {
        assert_eq!(tools.len(), 0);
        assert_eq!(resolved["tool_counts"], json!({}));
    }
}
