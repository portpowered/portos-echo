//! T3: a native peer and a fake gateway drive the production screen exchange.
use super::*;
use echo_contracts::endpoint::{MAX_SNAPSHOT_BYTES, REPORT_NAMESPACE};
use hmac::{Hmac, KeyInit as _, Mac as _};
use reqwest::{Client, Response, StatusCode};
use sha2::{Digest as _, Sha256};

const NONCE: &str = "0123456789abcdef0123456789abcdef";

struct Peer {
    http: Client,
    origin: String,
    events: Response,
    buffered: String,
}

impl Peer {
    async fn attach(server: &echo_screen_server::ScreenServer) -> Self {
        let origin = format!(
            "http://127.0.0.1:{}", // lint-rules: allow-literal port from the bound loopback camera listener
            server.bound_addr.port()
        );
        let http = Client::builder().no_proxy().build().unwrap();
        let address = format!("{origin}/camera/v1/events");
        let request = || http.get(&address).header("X-Echo-Camera-Nonce", NONCE);
        assert_eq!(
            request().header("Origin", &origin).send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(request().send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        let attach = || request().header("X-Echo-Camera-Proof", sign("app", &["attach"]));
        let events = attach().send().await.unwrap();
        assert_eq!(events.status(), StatusCode::OK);
        assert_eq!(events.headers()["cache-control"], "no-store");
        assert_eq!(attach().send().await.unwrap().status(), StatusCode::CONFLICT);
        let mut peer = Self {
            http,
            origin,
            events,
            buffered: String::new(),
        };
        assert_eq!(peer.event("ready").await, json!({}));
        peer
    }

    async fn event(&mut self, expected: &str) -> Value {
        loop {
            if let Some(end) = self.buffered.find("\n\n") {
                let mut lines = self.buffered.drain(..end + 2).collect::<String>();
                if lines.starts_with(':') {
                    continue;
                }
                lines.truncate(end);
                let field = |prefix| lines.lines().find_map(|line| line.strip_prefix(prefix)).unwrap();
                let name = field("event: ");
                let data = field("data: ");
                assert_eq!(name, expected);
                assert_eq!(field("id: "), sign("bridge", &["event", name, data]));
                return serde_json::from_str(data).unwrap();
            }
            let chunk = self.events.chunk().await.unwrap().unwrap();
            self.buffered.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    }

    async fn result(&self, id: &str, bytes: Vec<u8>, sampled_at: i64, authenticated: bool) -> StatusCode {
        let content = "image/jpeg";
        let time = sampled_at.to_string();
        let digest = hex::encode(Sha256::digest(&bytes));
        let proof = sign("app", &["result", id, content, "2", "1", &time, &digest]);
        let request = self
            .http
            .post(format!("{}/camera/v1/results/{id}", self.origin))
            .header("Content-Type", content)
            .header("X-Echo-Camera-Nonce", NONCE)
            .header("X-Echo-Camera-Width", "2")
            .header("X-Echo-Camera-Height", "1")
            .header("X-Echo-Camera-Sampled-At-Ms", time);
        let request = if authenticated {
            request.header("X-Echo-Camera-Proof", proof)
        } else {
            request.header("X-Echo-Camera-Proof", "0".repeat(64))
        };
        let response = request.body(bytes).send().await.unwrap();
        assert!(response.content_length().is_none_or(|bytes| bytes == 0));
        response.status()
    }
}

fn sign(direction: &str, fields: &[&str]) -> String {
    let key = Sha256::digest(SETTINGS_TOKEN.as_bytes());
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap();
    mac.update(
        [&["echo-screen-camera/v1", direction, NONCE], fields]
            .concat()
            .join("\n")
            .as_bytes(),
    );
    hex::encode(mac.finalize().into_bytes())
}

#[tokio::test(start_paused = true)]
async fn t3_endpoint_snapshot_native_peer_proof_cancellation_and_recovery() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(bridge_mcp::bridge_component::drive(async {
            let home = tempfile::tempdir().unwrap();
            let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
            mock.enable_endpoint_audience().unwrap();
            let mut grant = registered(&mock, home.path()).await;
            let mut gateway = Gateway::open(&mock).await;
            let (_states, input) = tokio::sync::mpsc::channel(8);
            let mut listener = echo_screen_server::serve_with_camera(
                ([127, 0, 0, 1], 0).into(),
                Secret::new(SETTINGS_TOKEN.into()),
                input,
                None,
                true,
            )
            .await
            .unwrap();
            let mut app = Peer::attach(&listener).await;
            let mut camera = CameraPeer::open();
            Arc::get_mut(&mut camera).unwrap().exchange = listener.camera();
            let fixture = fixture_with_camera(home.path(), &mock, &gateway, grant.grant.clone(), Some(camera)).await;
            fixture.settings.save(true, "Hall Echo").await;
            gateway.assert_upgrade(CLAIMED).await;
            let stale = observe_timeout(&gateway, &mock, &mut app).await;
            observe_fresh_capture(&gateway, &mock, &mut app, "native-retry", Some("BASE64"), &stale).await;
            observe_fresh_capture(&gateway, &mock, &mut app, "native-url", Some("URL"), &stale).await;
            observe_fresh_capture(&gateway, &mock, &mut app, "native-default", None, &stale).await;
            assert_private(home.path());
            fixture.settings.close().await;
            gateway.assert_disconnect().await;
            grant.close().await;
            listener.shutdown().await.unwrap();
        })))
        .await;
}

async fn observe_timeout(gateway: &Gateway, mock: &Mock, app: &mut Peer) -> String {
    send(gateway, &snapshot_request("native-timeout", "BASE64")).await;
    let capture = app.event("capture").await;
    assert_eq!(capture["cameraId"], "0");
    assert_eq!(capture["remainingMs"], 3000);
    let stale = capture["captureId"].as_str().unwrap();
    advance(Duration::from_millis(2999)).await;
    assert!(!command_tests::receipts(mock).iter().any(|reply| {
        reply
            .json
            .as_ref()
            .is_some_and(|body| body["payload"]["requestId"] == "native-timeout")
    }));
    advance(Duration::from_millis(1)).await;
    assert_eq!(app.event("cancel").await, json!({"captureId":stale}));
    assert_reply(mock, "native-timeout", "TIMEOUT").await;
    let jpeg = STANDARD.decode(JPEG).unwrap();
    assert_eq!(
        app.result(stale, jpeg, sampled_at_ms(), true).await,
        StatusCode::CONFLICT
    );
    stale.into()
}

async fn observe_fresh_capture(
    gateway: &Gateway,
    mock: &Mock,
    app: &mut Peer,
    request_id: &str,
    format: Option<&str>,
    stale: &str,
) {
    let body = format.map_or_else(|| json!({}), |format| json!({"responseFormat":format}));
    send(gateway, &command("get-snapshot", request_id, DEVICE, &body)).await;
    let capture = app.event("capture").await;
    let id = capture["captureId"].as_str().unwrap();
    let jpeg = STANDARD.decode(JPEG).unwrap();
    let sampled_at = sampled_at_ms();
    assert_ne!(id, stale);
    assert_eq!(
        app.result(id, jpeg.clone(), sampled_at, false).await,
        StatusCode::UNAUTHORIZED
    );
    if request_id == "native-retry" {
        let oversize = vec![0; MAX_SNAPSHOT_BYTES + 1];
        assert_eq!(
            app.result(id, oversize, sampled_at, true).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
    assert_eq!(
        app.result(id, jpeg.clone(), sampled_at, true).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(app.result(id, jpeg, sampled_at, true).await, StatusCode::CONFLICT);
    let _ = outcome(mock, request_id).await;
    assert_report(mock, request_id, sampled_at);
}

fn assert_report(mock: &Mock, request_id: &str, sampled_at: i64) {
    let replies: Vec<_> = command_tests::receipts(mock)
        .into_iter()
        .filter_map(|request| request.json)
        .filter(|body| body["payload"]["requestId"] == request_id)
        .collect();
    assert_eq!(
        replies,
        [json!({
            "header":{"namespace":REPORT_NAMESPACE,"name":"report-state","version":"1.0"},
            "target":{"type":"ROUTE","value":DEVICE},
            "payload":{"requestId":request_id,"sampledAtMs":sampled_at,"snapshot":{"width":2,"height":1,"data":JPEG}}
        })]
    );
}

fn assert_private(home: &Path) {
    let raw = STANDARD.decode(JPEG).unwrap();
    for leaf in ["settings-trace", "trace", "voice-trace"] {
        let trace = std::fs::read(home.join(leaf)).unwrap();
        assert!(!trace.windows(raw.len()).any(|window| window == raw));
        assert!(!trace.windows(JPEG.len()).any(|window| window == JPEG.as_bytes()));
    }
}
