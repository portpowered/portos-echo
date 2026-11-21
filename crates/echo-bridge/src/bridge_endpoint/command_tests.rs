use super::*;
use echo_contracts::endpoint::{CAPABILITY_PREFIX, REPORT_NAMESPACE};
use echo_mcp_auth::mock::{OTHER_DEVICE, Request as HttpRequest};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test(start_paused = true)]
async fn t3_endpoint_command_replies_are_correlated_and_deduplicated_across_reconnect() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open(&mock).await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        settings.save(true, "Hall Echo").await;
        gateway.assert_upgrade(CLAIMED).await;
        assert_installed(home.path(), &mock).await;

        let start = command(
            "start-session",
            "first",
            DEVICE,
            &json!({"reason":"synthetic","initialText":"hello"}),
        );
        let opened = Instant::now();
        send(&gateway, &start).await;
        assert_reply(&mock, "first", "NOT_READY").await;
        assert!(opened.elapsed() <= Duration::from_secs(1));
        assert_invalid_routing(&gateway, &mock).await;

        gateway.frames.send(Message::Close(None)).await.unwrap();
        gateway.assert_disconnect().await;
        setup_tests::wait_warning(&settings, "Connecting this Echo to Port OS.").await;
        advance(Duration::from_secs(2)).await;
        gateway.assert_upgrade(CLAIMED).await;
        send(&gateway, &start).await;
        assert_unsupported_command(&gateway, &mock).await;
        assert_unknown_end(&gateway, &mock).await;
        assert_cycle(&mut gateway, &mock).await;

        settings.close().await;
        gateway.assert_disconnect().await;
        grant.close().await;
        assert_unique_replies(&mock);
        assert_no_refusals(&mock);
    })
    .await;
}

/// Close 4000 ends the connection's hour: reconnect at once with a fresh token.
async fn assert_cycle(gateway: &mut Gateway, mock: &Mock) {
    gateway.cycle().await;
    gateway.assert_disconnect().await;
    gateway.assert_upgrade("endpoint-access-5").await;
    send(
        gateway,
        &command("end-session", "cycled", DEVICE, &json!({"sessionId":"missing"})),
    )
    .await;
    let reply = assert_reply(mock, "cycled", "NOT_FOUND").await;
    assert_eq!(reply.headers["authorization"], "Bearer endpoint-access-5");
}

async fn assert_invalid_routing(gateway: &Gateway, mock: &Mock) {
    send(
        gateway,
        &command("start-session", "wrong", OTHER_DEVICE, &json!({"reason":"synthetic"})),
    )
    .await;
    send(gateway, &command("end-session", "invalid", DEVICE, &json!([]))).await;
    assert_reply(mock, "invalid", "INVALID_REQUEST").await;
}

async fn assert_unsupported_command(gateway: &Gateway, mock: &Mock) {
    send(gateway, &command("future", "unknown", DEVICE, &json!({}))).await;
    assert_reply(mock, "unknown", "UNSUPPORTED_COMMAND").await;
}

async fn assert_unknown_end(gateway: &Gateway, mock: &Mock) {
    send(
        gateway,
        &command("end-session", "last", DEVICE, &json!({"sessionId":"missing"})),
    )
    .await;
    assert_reply(mock, "last", "NOT_FOUND").await;
}

fn assert_unique_replies(mock: &Mock) {
    let mut ids: Vec<_> = receipts(mock)
        .into_iter()
        .map(|request| {
            request.json.unwrap()["payload"]["requestId"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["cycled", "first", "invalid", "last", "unknown"]);
}

/// One live gateway frame. The interface follows the command name.
pub(super) fn command(name: &str, id: &str, device_id: &str, body: &Value) -> Value {
    let interface = match name {
        "set-volume" | "adjust-volume" | "set-mute" => "speaker",
        "get-snapshot" => "snapshot",
        _ => "assistant-session",
    };
    json!({
        "header":{"namespace":format!("{CAPABILITY_PREFIX}{interface}"),"name":name,"version":"1.0","id":id},
        "body":body,
        "context":{"deviceId":device_id,"sentAt":"2026-11-21T00:00:00Z"}
    })
}

pub(super) async fn send(gateway: &Gateway, frame: &Value) {
    gateway.frames.send(Message::text(frame.to_string())).await.unwrap();
}

/// The single report answering `id` refused it with `code` and no message.
pub(super) async fn assert_reply(mock: &Mock, id: &str, code: &str) -> HttpRequest {
    let reply = loop {
        if let Some(reply) = receipts(mock).into_iter().find(|request| {
            request
                .json
                .as_ref()
                .is_some_and(|body| body["payload"]["requestId"] == id)
        }) {
            break reply;
        }
        tokio::task::yield_now().await;
    };
    assert!(
        reply.headers["authorization"].starts_with("Bearer endpoint-access-"),
        "REST uses the endpoint audience"
    );
    assert!(!reply.headers.contains_key("dpop"));
    assert!(reply.headers.contains_key("idempotency-key"));
    assert_eq!(
        reply.json.clone().unwrap(),
        json!({
            "header":{"namespace":REPORT_NAMESPACE,"name":"report-state","version":"1.0"},
            "target":{"type":"ROUTE","value":DEVICE},
            "payload":{"requestId":id,"error":{"code":code}}
        })
    );
    reply
}

pub(super) fn receipts(mock: &Mock) -> Vec<HttpRequest> {
    mock.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|request| {
            request.path == "/messages"
                && request
                    .json
                    .as_ref()
                    .is_some_and(|body| body["payload"]["requestId"].is_string())
        })
        .cloned()
        .collect()
}
