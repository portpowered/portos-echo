//! T3: public `ToolHost` -> real HTTP -> isolated embedded counterpart.
#![cfg(test)]
use echo_agent_api::{ToolError, ToolHost};
use echo_config::{McpAuth, McpServer};
use echo_contracts::secret::Secret;
use echo_mcp_client::{McpClient, TokenError, TokenProvider};
use echo_spoof_mcp::{Fault, Options, Spoof};
use serde_json::{Value, json};
use std::io::Write;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct Provider {
    value: Arc<Mutex<String>>,
    uses: Arc<AtomicUsize>,
}
impl TokenProvider for Provider {
    fn token(&self) -> impl Future<Output = Result<Secret<String>, TokenError>> {
        self.uses.fetch_add(1, Ordering::Relaxed);
        std::future::ready(Ok(Secret::new(self.value.lock().unwrap().clone())))
    }
}
fn synthetic() -> String {
    format!(
        "local-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
fn config(url: &str, allowed: &[&str]) -> McpServer {
    McpServer {
        name: "local".into(),
        url: url.into(),
        auth: McpAuth::None,
        bearer_file: None,
        allow_tools: allowed.iter().map(|s| (*s).into()).collect(),
    }
}
fn client(spoof: &Spoof, allowed: &[&str], value: Arc<Mutex<String>>) -> McpClient<Provider> {
    McpClient::new(
        config(spoof.url(), allowed),
        Provider {
            value,
            uses: Arc::default(),
        },
        Duration::from_secs(5),
    )
    .unwrap()
}
fn text(value: &str) -> Value {
    json!([{"type":"text","text":value}])
}
const TOOLS: [&str; 4] = ["test.echo", "test.wait", "test.requires_approval", "ui.show_emotion"];

#[tokio::test]
async fn t3_f01_f02_f04_f12_acceptance_witness() {
    let token = synthetic();
    let secret = Arc::new(Mutex::new(token.clone()));
    let spoof = Spoof::start(Options {
        expected_bearer: Some(Secret::new(token)),
        ..Options::default()
    })
    .await
    .unwrap();
    let host = client(&spoof, &TOOLS, secret.clone());
    let tools = host.list_tools().await.unwrap();
    assert_eq!(tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), TOOLS);
    assert_eq!(tools[0].description, "Echo text");
    assert_eq!(
        tools[0].input_schema,
        json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false})
    );
    let result = host
        .call_tool("test.echo", json!({"text":"hello"}), "key-1")
        .await
        .unwrap();
    assert_eq!((result.content, result.is_error), (text("hello"), false));
    let snapshot = spoof.snapshot().unwrap();
    assert_initial_snapshot(&snapshot);
    let secret_matches = format!("{snapshot:?}").matches(secret.lock().unwrap().as_str()).count();
    assert_eq!(secret_matches, 0);
    let denied = client(&spoof, &[], secret.clone());
    let before = spoof.snapshot().unwrap().requests.len();
    assert_eq!(
        denied.call_tool("test.echo", json!({"text":"no"}), "k").await,
        Err(ToolError::Denied)
    );
    let refused = spoof.snapshot().unwrap().requests.len() - before;
    assert_eq!(refused, 0);
    spoof.set_fault(Fault::Http500).unwrap();
    assert_eq!(
        host.call_tool("test.echo", json!({"text":"fail"}), "k").await,
        Err(ToolError::Server("http_status:500".into())) // lint-rules: allow-literal fixed HTTP error assertion, never a network address
    );
    let failed = spoof.snapshot().unwrap().calls.len() - snapshot.calls.len();
    assert_eq!(failed, 1);
    spoof.set_fault(Fault::None).unwrap();
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 2);
    writeln!(
        std::io::stdout(),
        "tools={} echo_calls={} refused_requests={} http500_attempts={} retries=0 secret_matches={}",
        tools.len(),
        snapshot.calls.len(),
        refused,
        failed,
        secret_matches
    )
    .unwrap();
    spoof.shutdown().await.unwrap();
}

#[tokio::test]
async fn t3_f02_f03_refusal_precedes_provider_and_is_server_local() {
    let a = Spoof::start(Options::default()).await.unwrap();
    let b = Spoof::start(Options::default()).await.unwrap();
    let uses = Arc::new(AtomicUsize::new(0));
    let value = Arc::new(Mutex::new(synthetic()));
    for allowed in [&[][..], &["test.wait"][..]] {
        let host = McpClient::new(
            config(b.url(), allowed),
            Provider {
                value: value.clone(),
                uses: uses.clone(),
            },
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            host.call_tool("test.echo", json!({"text":"no"}), "k").await,
            Err(ToolError::Denied)
        );
    }
    assert_eq!(uses.load(Ordering::Relaxed), 0);
    let host = client(&a, &["test.echo"], value);
    assert_eq!(
        host.call_tool("test.echo", json!({"text":"yes"}), "k")
            .await
            .unwrap()
            .content,
        text("yes")
    );
    assert_eq!(a.snapshot().unwrap().calls.len(), 1);
    assert_eq!(b.snapshot().unwrap(), echo_spoof_mcp::Snapshot::default());
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
}

#[tokio::test]
async fn t3_f05_rotation_401_has_one_attempt_and_no_refresh() {
    let value = Arc::new(Mutex::new(synthetic()));
    let expected = value.lock().unwrap().clone();
    let spoof = Spoof::start(Options {
        expected_bearer: Some(Secret::new(expected)),
        ..Options::default()
    })
    .await
    .unwrap();
    let host = client(&spoof, &TOOLS, value.clone());
    host.list_tools().await.unwrap();
    *value.lock().unwrap() = synthetic();
    assert_eq!(
        host.call_tool("test.echo", json!({"text":"hello"}), "k").await,
        Err(ToolError::Unauthorized)
    );
    let snapshot = spoof.snapshot().unwrap();
    assert_eq!(snapshot.requests.len(), 4);
    assert_eq!(snapshot.calls.len(), 0);
    assert_eq!(snapshot.requests[3].authorization_matches, Some(false));
    spoof.shutdown().await.unwrap();
}

#[tokio::test]
async fn t3_f06_f07_rpc_and_malformed_responses_are_safe() {
    for (fault, name, error) in [
        (Fault::None, "test.missing", ToolError::Server("json_rpc:-32602".into())),
        (Fault::MalformedJsonRpc, "test.echo", ToolError::Transport),
        (Fault::Http500, "test.echo", ToolError::Server("http_status:500".into())), // lint-rules: allow-literal fixed HTTP error assertion, never a network address
    ] {
        let spoof = Spoof::start(Options {
            fault,
            ..Options::default()
        })
        .await
        .unwrap();
        let host = client(&spoof, &[name], Arc::new(Mutex::new(synthetic())));
        assert_eq!(host.call_tool(name, json!({}), "k").await, Err(error));
        let snapshot = spoof.snapshot().unwrap();
        assert_eq!(snapshot.requests.len(), 3);
        assert_eq!(snapshot.calls.len(), 1);
        spoof.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn t3_f09_f10_f11_f13_exact_results_and_explicit_repeats() {
    let spoof = Spoof::start(Options::default()).await.unwrap();
    let host = client(&spoof, &TOOLS, Arc::new(Mutex::new(synthetic())));
    for (name, args, expected, is_error) in [
        ("test.wait", json!({"ms":0}), "waited 0 ms", false),
        ("test.requires_approval", json!({}), "approval_required", true),
        ("ui.show_emotion", json!({"name":"happy"}), "happy", false),
        ("test.echo", json!({"text":"repeat"}), "repeat", false),
        ("test.echo", json!({"text":"repeat"}), "repeat", false),
    ] {
        let result = host.call_tool(name, args.clone(), "key-1").await.unwrap();
        assert_eq!((result.content, result.is_error), (text(expected), is_error));
        let records = spoof.snapshot().unwrap().calls;
        assert_eq!(
            (&records.last().unwrap().name, &records.last().unwrap().args),
            (&name.to_owned(), &args)
        );
    }
    let snapshot = spoof.snapshot().unwrap();
    assert_eq!(snapshot.calls.len(), 5);
    assert_eq!(snapshot.calls[4].count, 2);
    assert_eq!(
        snapshot.requests.iter().filter_map(|r| r.id).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6]
    );
    spoof.shutdown().await.unwrap();
}

#[tokio::test]
async fn t3_f14_concurrent_initialization_correlates_results() {
    let spoof = Spoof::start(Options::default()).await.unwrap();
    let host = client(&spoof, &TOOLS, Arc::new(Mutex::new(synthetic())));
    let (a, b) = tokio::join!(
        host.call_tool("test.echo", json!({"text":"parallel"}), "a"),
        host.call_tool("test.wait", json!({"ms":0}), "b")
    );
    assert_eq!(a.unwrap().content, text("parallel"));
    assert_eq!(b.unwrap().content, text("waited 0 ms"));
    let snapshot = spoof.snapshot().unwrap();
    assert_eq!(snapshot.requests.iter().filter(|r| r.method == "initialize").count(), 1);
    assert_eq!(snapshot.calls.len(), 2);
    assert!(snapshot.calls.iter().all(|c| c.count == 1));
    spoof.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn t3_f08_timeout_after_arrival_is_not_replayed() {
    let clock = manual_clock();
    let spoof = Spoof::start(Options {
        fault: Fault::Slow,
        slow_ms: 10_000,
        ..Options::default()
    })
    .await
    .unwrap();
    let host = Arc::new(client(&spoof, &TOOLS, Arc::new(Mutex::new(synthetic()))));
    host.list_tools().await.unwrap();
    let task = tokio::spawn(async move { host.call_tool("test.echo", json!({"text":"late"}), "k").await });
    spoof.wait_for_calls(1).await.unwrap();
    tokio::time::advance(Duration::from_millis(4999)).await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(task.await.unwrap(), Err(ToolError::Timeout));
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 1);
    spoof.shutdown().await.unwrap();
    clock.abort();
    assert!(clock.await.unwrap_err().is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn t3_f09_wait_completes_at_seven_virtual_milliseconds() {
    let clock = manual_clock();
    let spoof = Spoof::start(Options::default()).await.unwrap();
    let host = Arc::new(client(&spoof, &TOOLS, Arc::new(Mutex::new(synthetic()))));
    host.list_tools().await.unwrap();
    let task = tokio::spawn(async move { host.call_tool("test.wait", json!({"ms":7}), "k").await });
    spoof.wait_for_calls(1).await.unwrap();
    tokio::time::advance(Duration::from_millis(6)).await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(task.await.unwrap().unwrap().content, text("waited 7 ms"));
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 1);
    spoof.shutdown().await.unwrap();
    clock.abort();
    assert!(clock.await.unwrap_err().is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn t3_f15_cancellation_joins_handlers_without_replay() {
    let clock = manual_clock();
    let spoof = Spoof::start(Options {
        fault: Fault::Slow,
        slow_ms: 10_000,
        ..Options::default()
    })
    .await
    .unwrap();
    let host = Arc::new(client(&spoof, &TOOLS, Arc::new(Mutex::new(synthetic()))));
    host.list_tools().await.unwrap();
    let task = tokio::spawn({
        let host = host.clone();
        async move { host.call_tool("test.echo", json!({"text":"cancel"}), "k").await }
    });
    spoof.wait_for_calls(1).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    spoof.set_fault(Fault::None).unwrap();
    assert_eq!(
        host.call_tool("test.echo", json!({"text":"next"}), "k")
            .await
            .unwrap()
            .content,
        text("next")
    );
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 2);
    spoof.shutdown().await.unwrap();
    clock.abort();
    assert!(clock.await.unwrap_err().is_cancelled());
}

fn assert_initial_snapshot(snapshot: &echo_spoof_mcp::Snapshot) {
    assert_eq!(
        snapshot.calls,
        vec![echo_spoof_mcp::CallRecord {
            name: "test.echo".into(),
            args: json!({"text":"hello"}),
            count: 1
        }]
    );
    assert_eq!(
        snapshot
            .requests
            .iter()
            .map(|r| (r.method.as_str(), r.id))
            .collect::<Vec<_>>(),
        [
            ("initialize", Some(1)),
            ("notifications/initialized", None),
            ("tools/list", Some(2)),
            ("tools/call", Some(3))
        ]
    );
    assert!(
        snapshot
            .requests
            .iter()
            .all(|r| r.authorization_matches == Some(true) && !r.session_header && !r.dpop_header)
    );
}

// Keep auto-advance disabled while loopback I/O is pending. Only explicit advances move time.
fn manual_clock() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    })
}
