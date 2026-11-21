use super::*;
use echo_mcp_auth::mock::Request as HttpRequest;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test(start_paused = true)]
async fn t3_endpoint_command_replies_are_correlated_and_deduplicated_across_reconnect() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open().await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        settings.save(true, "Hall Echo").await;
        gateway.assert_upgrade(4).await;
        assert_installed(home.path(), &mock).await;

        let start = command(
            "start-session",
            "first",
            ROUTE,
            &json!({"reason":"synthetic","initialText":"hello"}),
        );
        let opened = Instant::now();
        send(&gateway, &start).await;
        assert_reply(
            &mock,
            "first",
            "start-session",
            "NOT_READY",
            "The voice provider is unavailable.",
        )
        .await;
        assert!(opened.elapsed() <= Duration::from_secs(1));
        assert_invalid_routing(&gateway, &mock).await;

        gateway.frames.send(Message::Close(None)).await.unwrap();
        gateway.assert_disconnect().await;
        setup_tests::wait_warning(&settings, "Connecting this Echo to Port OS.").await;
        advance(Duration::from_secs(2)).await;
        gateway.assert_upgrade(4).await;
        send(&gateway, &start).await;
        assert_unsupported_command(&gateway, &mock).await;
        assert_unknown_end(&gateway, &mock).await;

        settings.close().await;
        gateway.assert_disconnect().await;
        grant.close().await;
        assert_unique_replies(&mock);
    })
    .await;
}

async fn assert_invalid_routing(gateway: &Gateway, mock: &Mock) {
    send(
        gateway,
        &command("start-session", "wrong", "other-route", &json!({"reason":"synthetic"})),
    )
    .await;
    send(gateway, &command("end-session", "invalid", ROUTE, &json!([]))).await;
    assert_reply(
        mock,
        "invalid",
        "end-session",
        "INVALID_REQUEST",
        "The command payload is invalid.",
    )
    .await;
}

async fn assert_unsupported_command(gateway: &Gateway, mock: &Mock) {
    send(gateway, &command("future", "unknown", ROUTE, &json!({}))).await;
    assert_reply(
        mock,
        "unknown",
        "future",
        "UNSUPPORTED_COMMAND",
        "This command is unsupported.",
    )
    .await;
}

async fn assert_unknown_end(gateway: &Gateway, mock: &Mock) {
    send(
        gateway,
        &command("end-session", "last", ROUTE, &json!({"sessionId":"missing"})),
    )
    .await;
    assert_reply(
        mock,
        "last",
        "end-session",
        "NOT_FOUND",
        "The voice session was not found.",
    )
    .await;
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
    assert_eq!(ids, ["first", "invalid", "last", "unknown"]);
}

pub(super) fn command(name: &str, id: &str, route: &str, payload: &Value) -> Value {
    json!({
        "header":{"namespace":"assistant-session","name":name,"version":"1.0","messageId":id},
        "target":{"type":"ROUTE","value":route},
        "payload":payload
    })
}

pub(super) async fn send(gateway: &Gateway, frame: &Value) {
    gateway.frames.send(Message::text(frame.to_string())).await.unwrap();
}

pub(super) async fn assert_reply(mock: &Mock, id: &str, name: &str, code: &str, message: &str) {
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
    assert_eq!(reply.headers["authorization"], "Bearer endpoint-access-4");
    assert!(!reply.headers.contains_key("dpop"));
    assert!(reply.headers.contains_key("idempotency-key"));
    assert_eq!(
        reply.json.unwrap(),
        json!({
            "header":{"namespace":echo_contracts::endpoint::ECHO_PLUGIN,"name":name,"version":"1.0"},
            "target":{"type":"ROUTE","value":ROUTE},
            "payload":{"requestId":id,"outcome":{"ok":false,"failure":{"code":code,"message":message}}}
        })
    );
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
