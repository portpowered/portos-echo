//! T3: HTTP rejection boundaries and retained capacity, isolated per test.
#![cfg(test)]
use echo_spoof_mcp::{Fault, Options, Spoof, SpoofError};
use serde_json::{Value, json};
use std::time::Duration;
fn call(id: u64, args: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"test.echo","arguments":args}})
}
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

#[tokio::test]
async fn t3_f19_invalid_arguments_origins_body_and_record_capacity() {
    let spoof = Spoof::start(Options {
        max_records: 1,
        ..Options::default()
    })
    .await
    .unwrap();
    let http = http();
    let invalid = http
        .post(spoof.url())
        .json(&call(7, &json!({"wrong":0})))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        invalid,
        json!({"jsonrpc":"2.0","id":7,"error":{"code":-32602,"message":"invalid tool arguments"}})
    );
    let origin = http
        .post(spoof.url())
        .header("Origin", "http://127.0.0.1")
        .json(&call(8, &json!({"text":"no"})))
        .send()
        .await
        .unwrap();
    assert_eq!(origin.status(), 403);
    let oversized = http.post(spoof.url()).body(vec![b'x'; 65_537]).send().await.unwrap();
    assert_eq!(oversized.status(), 413);
    let capacity = http
        .post(spoof.url())
        .json(&call(9, &json!({"text":"no"})))
        .send()
        .await
        .unwrap();
    assert_eq!(capacity.status(), 503);
    let snapshot = spoof.snapshot().unwrap();
    assert_eq!(snapshot.calls.len(), 1);
    assert_eq!(snapshot.calls[0].args, json!({"wrong":0}));
    spoof.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn t3_f19_concurrency_limit_refuses_without_executing() {
    let spoof = Spoof::start(Options {
        max_in_flight: 1,
        fault: Fault::Slow,
        slow_ms: 10_000,
        ..Options::default()
    })
    .await
    .unwrap();
    let url = spoof.url().to_owned();
    let task = tokio::spawn(async move { http().post(url).json(&call(1, &json!({"text":"pending"}))).send().await });
    spoof.wait_for_calls(1).await.unwrap();
    let response = http()
        .post(spoof.url())
        .json(&call(2, &json!({"text":"refused"})))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 1);
    task.abort();
    let _ = task.await;
    spoof.shutdown().await.unwrap();
}

#[tokio::test]
async fn t3_fault_header_override_and_invalid_selector() {
    let spoof = Spoof::start(Options::default()).await.unwrap();
    let http = http();
    let fault = http
        .post(spoof.url())
        .header("X-Echo-Mcp-Fault", "http_500")
        .json(&call(1, &json!({"text":"fault"})))
        .send()
        .await
        .unwrap();
    assert_eq!(fault.status(), 500);
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 1);
    let invalid = http
        .post(spoof.url())
        .header("X-Echo-Mcp-Fault", "unknown")
        .json(&call(2, &json!({"text":"invalid"})))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 400);
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 1);
    let bad = http.post(spoof.url()).body("invalid").send().await.unwrap();
    assert_eq!(bad.status(), 400);
    let envelope = http.post(spoof.url()).json(&json!({})).send().await.unwrap();
    assert_eq!(envelope.status(), 400);
    spoof.shutdown().await.unwrap();
}

#[tokio::test]
async fn t3_invalid_options_and_busy_port_return_fixed_errors() {
    for options in [
        Options {
            max_records: 0,
            ..Options::default()
        },
        Options {
            max_records: 4097,
            ..Options::default()
        },
        Options {
            max_in_flight: 0,
            ..Options::default()
        },
        Options {
            max_in_flight: 65,
            ..Options::default()
        },
    ] {
        assert!(matches!(Spoof::start(options).await, Err(SpoofError::Configuration)));
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    assert!(matches!(
        Spoof::start(Options {
            port: listener.local_addr().unwrap().port(),
            ..Options::default()
        })
        .await,
        Err(SpoofError::Bind)
    ));
}

#[tokio::test(start_paused = true)]
async fn t3_f19_unsigned_wait_has_bounded_resource_deadline_without_overflow() {
    let clock = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let spoof = Spoof::start(Options::default()).await.unwrap();
    let url = spoof.url().to_owned();
    let task = tokio::spawn(async move {
        reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(61)).build().unwrap()
            .post(url).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"test.wait","arguments":{"ms":u64::MAX}}}))
            .send().await.unwrap().status()
    });
    spoof.wait_for_calls(1).await.unwrap();
    tokio::time::advance(Duration::from_secs(59)).await;
    assert!(!task.is_finished());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(task.await.unwrap(), 503);
    assert_eq!(spoof.snapshot().unwrap().calls.len(), 1);
    spoof.shutdown().await.unwrap();
    clock.abort();
    assert!(clock.await.unwrap_err().is_cancelled());
}
