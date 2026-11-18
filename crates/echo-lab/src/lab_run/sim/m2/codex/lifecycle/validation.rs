//! T1 classifier checks and T3 negative peer execution on isolated loopback sockets.
use super::*;

fn end() -> Value {
    json!({"type":"hassmic-end", "data":null, "data_length":0, "payload_length":0})
}

#[test]
fn explicit_end_requires_canonical_header_and_empty_payload() {
    assert!(peer::valid_control(&end(), &[]));
    for (field, wrong) in [
        ("type", json!("unexpected")),
        ("data", json!({})),
        ("data_length", json!(1)),
        ("payload_length", json!(1)),
    ] {
        let mut frame = end();
        frame[field] = wrong;
        assert!(!peer::valid_control(&frame, &[]), "{frame}");
    }
    assert!(!peer::valid_control(&end(), &[1]));
    let mut missing = end();
    missing.as_object_mut().unwrap().remove("data");
    assert!(!peer::valid_control(&missing, &[]));
    for code in ["echo-unavailable", "echo-killed", "", "echo-kill-switch"] {
        let row = json!({"type":"error", "data":{"code":code,"text":"refusal"},
            "data_length":0, "payload_length":0});
        assert_eq!(peer::valid_control(&row, &[]), code == "echo-kill-switch");
    }
}

#[tokio::test]
async fn t3_codex_lifecycle_malformed_controls_fail_actual_peer_and_release_listener() {
    tokio::time::timeout(DEADLINE, async {
        for (kind, data, payload) in [
            ("unexpected", Value::Null, vec![]),
            ("hassmic-end", json!({}), vec![]),
            ("hassmic-end", Value::Null, vec![1]),
            ("error", json!({"code":"echo-unavailable","text":"refusal"}), vec![]),
        ] {
            rejected_peer(kind, data, payload).await;
        }
    })
    .await
    .unwrap();
}

async fn rejected_peer(kind: &str, data: Value, payload: Vec<u8>) {
    use super::super::tests::{read, send};
    let (listener, lifetime) = observed_listener().await;
    let address = listener.local_addr().unwrap();
    let (_wake, requested) = mpsc::channel(1);
    let task = tokio::spawn(satellite(listener, requested, false));
    let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
    send(&mut client, "run-satellite", json!({}), vec![]).await;
    send(&mut client, "describe", json!({}), vec![]).await;
    assert_eq!(read(&mut client).await.0["type"], "info");
    send(&mut client, kind, data, payload).await;
    assert!(
        task.await.unwrap_err().is_panic(),
        "invalid control cannot collect successfully"
    );
    drop(client);
    assert!(lifetime.upgrade().is_none());
}
