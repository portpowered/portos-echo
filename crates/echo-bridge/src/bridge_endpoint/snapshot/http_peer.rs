//! T3: a native peer and a fake gateway drive the production screen exchange.
use super::*;
use echo_contracts::endpoint::{MAX_SNAPSHOT_BYTES, REPORT_NAMESPACE};
use hmac::{Hmac, KeyInit as _, Mac as _};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    net::TcpStream,
};

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

    fn upload(&self, id: &str, bytes: &[u8], sampled_at: i64, authenticated: bool) -> RequestBuilder {
        let content = "image/jpeg";
        let time = sampled_at.to_string();
        let digest = hex::encode(Sha256::digest(bytes));
        let proof = sign("app", &["result", id, content, "2", "1", &time, &digest]);
        let request = self
            .http
            .post(format!("{}/camera/v1/results/{id}", self.origin))
            .header("Content-Type", content)
            .header("X-Echo-Camera-Nonce", NONCE)
            .header("X-Echo-Camera-Width", "2")
            .header("X-Echo-Camera-Height", "1")
            .header("X-Echo-Camera-Sampled-At-Ms", time);
        if authenticated {
            request.header("X-Echo-Camera-Proof", proof)
        } else {
            request.header("X-Echo-Camera-Proof", "0".repeat(64))
        }
    }

    async fn result(&self, id: &str, bytes: Vec<u8>, sampled_at: i64, authenticated: bool) -> StatusCode {
        let response = self
            .upload(id, &bytes, sampled_at, authenticated)
            .body(bytes)
            .send()
            .await
            .unwrap();
        assert!(response.content_length().is_none_or(|bytes| bytes == 0));
        response.status()
    }

    async fn stall_upload(&self, id: &str, bytes: &[u8], sampled_at: i64) -> TcpStream {
        use std::fmt::Write as _;
        let request = self.upload(id, bytes, sampled_at, true).build().unwrap();
        let authority = self.origin.strip_prefix("http://").unwrap();
        let mut socket = TcpStream::connect(authority).await.unwrap();
        let mut headers = format!(
            "POST {} HTTP/1.1\r\nHost: {authority}\r\nContent-Length: {}\r\nConnection: close\r\n",
            request.url().path(),
            bytes.len()
        );
        for (name, value) in request.headers() {
            write!(headers, "{name}: {}\r\n", value.to_str().unwrap()).unwrap();
        }
        socket.write_all(headers.as_bytes()).await.unwrap();
        socket.write_all(b"\r\n").await.unwrap();
        socket.write_all(&bytes[..1]).await.unwrap();
        // A wrong-proof probe cannot complete the capture. A conflict means the
        // stalled request already holds the live result slot.
        while self.result(id, bytes.to_vec(), sampled_at, false).await != StatusCode::CONFLICT {}
        socket
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
            let mut camera = CameraPeer::open();
            Arc::get_mut(&mut camera).unwrap().exchange = listener.camera();
            let fixture =
                fixture_with_camera(home.path(), &mock, &gateway, grant.grant.clone(), Some(camera.clone())).await;
            fixture.settings.save(true, "Hall Echo").await;
            gateway.assert_upgrade(CLAIMED).await;
            let mut app = observe_attach(&gateway, &mock, &listener, home.path()).await;
            let stale = observe_timeout(&gateway, &mock, &mut app).await;
            observe_fresh_capture(&gateway, &mock, &mut app, "native-retry", Some("BASE64"), &stale).await;
            observe_fresh_capture(&gateway, &mock, &mut app, "native-url", Some("URL"), &stale).await;
            observe_fresh_capture(&gateway, &mock, &mut app, "native-default", None, &stale).await;
            observe_cancelled_upload(&gateway, &mock, &mut app, &camera).await;
            observe_uplink_deadline(&gateway, &mock, &mut app).await;
            assert_private(home.path());
            fixture.settings.close().await;
            gateway.assert_disconnect().await;
            grant.close().await;
            listener.shutdown().await.unwrap();
        })))
        .await;
}

/// A camera path whose app has not attached is busy, not unsupported. The
/// trace follows the stream with fixed fields only.
async fn observe_attach(
    gateway: &Gateway,
    mock: &Mock,
    listener: &echo_screen_server::ScreenServer,
    home: &Path,
) -> Peer {
    let link = |session, refused| json!({"session":session,"refused":refused});
    let order = [
        link("detached", Value::Null),
        link("detached", json!("forbidden")),
        link("detached", json!("unauthorized")),
        link("attached", Value::Null),
        link("attached", json!("conflict")),
    ];
    assert_eq!(camera_links(home, &order[0]).await, [order[0].clone()]);
    send(gateway, &snapshot_request("unattached", "BASE64")).await;
    assert_reply(mock, "unattached", "CAMERA_BUSY").await;
    let app = Peer::attach(listener).await;
    // The observer may coalesce quick transitions, never reorder them.
    let links = camera_links(home, &order[4]).await;
    let mut expected = order.iter();
    assert_eq!(links.first(), Some(&order[0]));
    assert!(links.iter().all(|row| expected.any(|state| state == row)), "{links:?}");
    app
}

async fn camera_links(home: &Path, last: &Value) -> Vec<Value> {
    let read = || -> Vec<Value> {
        setup_tests::lifecycle(home)
            .into_iter()
            .filter(|row| row["event"] == "endpoint.camera")
            .map(|row| json!({"session":row["session"],"refused":row["refused"]}))
            .collect()
    };
    for _ in 0..300 {
        if read().last() == Some(last) {
            break;
        }
        advance(Duration::from_millis(10)).await;
    }
    let links = read();
    assert_eq!(links.last(), Some(last), "{links:?}");
    links
}

async fn observe_cancelled_upload(gateway: &Gateway, mock: &Mock, app: &mut Peer, camera: &CameraPeer) {
    send(gateway, &snapshot_request("native-aborted", "BASE64")).await;
    let capture = app.event("capture").await;
    let id = capture["captureId"].as_str().unwrap();
    let jpeg = STANDARD.decode(JPEG).unwrap();
    let start = Instant::now();
    let stalled = app.stall_upload(id, &jpeg, sampled_at_ms()).await;
    camera.shutter.send_replace(Shutter::Closed);
    assert_eq!(app.event("cancel").await, json!({"captureId":id}));
    assert_reply(mock, "native-aborted", "SHUTTERED").await;
    camera.shutter.send_replace(Shutter::Open);
    observe_fresh_capture(gateway, mock, app, "native-after-abort", None, id).await;
    let mut response = String::new();
    BufReader::new(stalled).read_line(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 409 "));
    assert!(start.elapsed() < Duration::from_secs(3));
}

async fn observe_uplink_deadline(gateway: &Gateway, mock: &Mock, app: &mut Peer) {
    let request_id = "native-uplink-deadline";
    // Fail before the mock accepts the report or consumes the command ID.
    mock.state.lock().unwrap().fault = Some(("/messages".into(), 503, json!({})));
    send(gateway, &snapshot_request(request_id, "BASE64")).await;
    let capture = app.event("capture").await;
    let start = Instant::now();
    assert_eq!(capture["remainingMs"], 3000);
    advance(Duration::from_millis(2800)).await;
    assert_eq!(
        app.result(
            capture["captureId"].as_str().unwrap(),
            STANDARD.decode(JPEG).unwrap(),
            sampled_at_ms(),
            true
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let first = outcome(mock, request_id).await;
    assert_eq!(first["snapshot"]["data"], JPEG);
    mock.state.lock().unwrap().fault = None;
    advance(Duration::from_millis(199)).await;
    assert_eq!(start.elapsed(), Duration::from_millis(2999));
    assert_eq!(receipt_attempts(mock, request_id).len(), 1);
    assert_uplink_timeout(mock, request_id, start).await;
}

async fn assert_uplink_timeout(mock: &Mock, request_id: &str, start: Instant) {
    let observe_until = start + Duration::from_millis(3300);
    while receipt_attempts(mock, request_id).len() == 1 {
        assert!(
            Instant::now() < observe_until,
            "missing uplink result at the snapshot deadline"
        );
        advance(Duration::from_millis(1)).await;
    }
    advance(Duration::from_secs(10)).await;
    let attempts = receipt_attempts(mock, request_id);
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].status, Some(503));
    assert!(attempts[0].received < start + Duration::from_secs(3));
    assert_eq!(attempts[1].status, Some(202));
    assert!(attempts[1].received >= start + Duration::from_secs(3));
    assert_eq!(
        attempts[1].json.as_ref().unwrap()["payload"],
        json!({"requestId":request_id,"error":{"code":"TIMEOUT"}})
    );
}

fn receipt_attempts(mock: &Mock, request_id: &str) -> Vec<echo_mcp_auth::mock::Request> {
    command_tests::receipts(mock)
        .into_iter()
        .filter(|request| request.json.as_ref().unwrap()["payload"]["requestId"] == request_id)
        .collect()
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
    assert_expired_upload_rejected(app, stale).await;
    assert_eq!(receipt_attempts(mock, "native-timeout").len(), 1);
    stale.into()
}

async fn assert_expired_upload_rejected(app: &Peer, stale: &str) {
    let jpeg = STANDARD.decode(JPEG).unwrap();
    let sampled_at = sampled_at_ms();
    assert_eq!(
        app.result(stale, jpeg.clone(), sampled_at, false).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(app.result(stale, jpeg, sampled_at, true).await, StatusCode::CONFLICT);
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
