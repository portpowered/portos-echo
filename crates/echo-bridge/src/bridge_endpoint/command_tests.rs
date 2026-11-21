use super::*;
use crate::bridge_endpoint::transport::KEEPALIVE;
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

/// Close 4000 ends the connection's hour: reconnect at once. The token
/// refreshes only when it is near expiry, and this one is not.
async fn assert_cycle(gateway: &mut Gateway, mock: &Mock) {
    let before = refreshes(mock);
    gateway.cycle().await;
    gateway.assert_disconnect().await;
    let closed = Instant::now();
    gateway.assert_upgrade(CLAIMED).await;
    assert!(
        closed.elapsed() < Duration::from_millis(100),
        "a cycle reconnects at once"
    );
    assert_eq!(refreshes(mock), before);
    send(
        gateway,
        &command("end-session", "cycled", DEVICE, &json!({"sessionId":"missing"})),
    )
    .await;
    let reply = assert_reply(mock, "cycled", "NOT_FOUND").await;
    assert_eq!(reply.headers["authorization"], format!("Bearer {CLAIMED}"));
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_keepalive_and_gateway_close_codes() {
    bridge_mcp::bridge_component::drive(async {
        let home = tempfile::tempdir().unwrap();
        let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
        mock.enable_endpoint_audience().unwrap();
        let mut grant = registered(&mock, home.path()).await;
        let mut gateway = Gateway::open(&mock).await;
        let settings = Settings::open(home.path(), &mock, &gateway, grant.grant.clone()).await;
        settings.save(true, "Hall Echo").await;
        gateway.assert_upgrade(CLAIMED).await;
        assert_keepalive(&gateway, &mock).await;

        // 4503 RETRY: back off, then the same token without a refresh.
        let refreshed = refreshes(&mock);
        gateway.close(4503).await;
        gateway.assert_disconnect().await;
        settle(Duration::from_millis(900)).await;
        assert!(gateway.received.try_recv().is_err(), "4503 backs off");
        settle(Duration::from_secs(1)).await;
        gateway.assert_upgrade(CLAIMED).await;
        assert_eq!(refreshes(&mock), refreshed);

        // 4403: one retry with a refreshed token, then stop.
        gateway.close(4403).await;
        gateway.assert_disconnect().await;
        settle(Duration::from_secs(5)).await;
        gateway.assert_upgrade("endpoint-access-5").await;
        assert_eq!(refreshes(&mock), refreshed + 1);
        gateway.close(4403).await;
        gateway.assert_disconnect().await;
        settle(Duration::from_millis(100)).await;
        setup_tests::wait_warning(&settings, UNAVAILABLE).await;
        settle(Duration::from_secs(120)).await;
        assert!(
            gateway.received.try_recv().is_err(),
            "no reconnect after the second 4403"
        );
        assert_eq!(refreshes(&mock), refreshed + 1);

        settings.close().await;
        grant.close().await;
        assert_no_refusals(&mock);
    })
    .await;
}

/// The device sends the text `ping` every 45 s; the `pong` is no command.
async fn assert_keepalive(gateway: &Gateway, mock: &Mock) {
    let connected = Instant::now();
    let pings = || gateway.pings.lock().unwrap().clone();
    while pings().len() < 2 {
        assert!(connected.elapsed() < Duration::from_secs(100), "keepalive pings");
        advance(Duration::from_millis(10)).await;
    }
    let pings = pings();
    let first = pings[0].duration_since(connected);
    let gap = pings[1].duration_since(pings[0]);
    assert!(
        first >= KEEPALIVE && first < KEEPALIVE + Duration::from_secs(1),
        "{first:?}"
    );
    assert!(gap >= KEEPALIVE && gap < KEEPALIVE + Duration::from_secs(1), "{gap:?}");
    send(
        gateway,
        &command("end-session", "after-pong", DEVICE, &json!({"sessionId":"missing"})),
    )
    .await;
    assert_reply(mock, "after-pong", "NOT_FOUND").await;
    assert_eq!(receipts(mock).len(), 1, "a pong is never answered as a command");
}

/// Advance fake time in small steps so socket work keeps up with it.
async fn settle(span: Duration) {
    let until = Instant::now() + span;
    while Instant::now() < until {
        advance(Duration::from_millis(10)).await;
    }
}

fn refreshes(mock: &Mock) -> usize {
    mock.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|request| {
            request
                .form
                .get("grant_type")
                .is_some_and(|grant| grant == "refresh_token")
        })
        .count()
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
