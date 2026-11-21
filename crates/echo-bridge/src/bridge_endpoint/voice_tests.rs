//! T3: the paired gateway and Wyoming adapter drive the shared voice service.
use super::{
    command_tests::{command, receipts, send},
    *,
};
use echo_contracts::wyoming::{Decoder, Event, Header, encode};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream},
    net::{TcpListener, TcpStream},
    sync::Notify,
};

#[tokio::test(start_paused = true)]
async fn t3_endpoint_session_shares_voice_admission_and_ends_the_device_run() {
    tokio::task::LocalSet::new()
        .run_until(Box::pin(bridge_mcp::bridge_component::drive(async {
            let home = tempfile::tempdir().unwrap();
            let mock = Mock::bind_at_epoch(EPOCH).await.unwrap();
            mock.enable_endpoint_audience().unwrap();
            let mut grant = registered(&mock, home.path()).await;
            let mut gateway = Gateway::open().await;
            let (service, send_settings) =
                Settings::configured(home.path(), &mock, &gateway, Arc::new(Resolver), false);
            let (app, server) = tokio::io::duplex(16384);
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let connector = echo_wyoming::adapter::connector(
                listener.local_addr().unwrap(),
                echo_wyoming::ClientOptions::default(),
            );
            let ready = Arc::new(Notify::new());
            let signal = ready.clone();
            let stale = Arc::new(Notify::new());
            let ordered = stale.clone();
            let shared_grant = grant.grant.clone();
            let settings = Settings::serve_with_owner(send_settings, |stop| {
                tokio::task::spawn_local(async move {
                    let work = async {
                        let result = crate::bridge_voice::endpoint_fixture(
                            connector,
                            app,
                            async {
                                let _ = stop.await;
                            },
                            (&signal, &ordered),
                        )
                        .await;
                        assert_eq!(result, Err(crate::Error::Control));
                    };
                    Box::pin(service.scope_with_grant(
                        work,
                        || shared_grant.link_status(),
                        || Some(shared_grant.clone()),
                    ))
                    .await;
                })
            })
            .await;
            let (device, _) = listener.accept().await.unwrap();
            let (mut device, mut app) = (Device::new(device), BufReader::new(server));
            device.receive("run-satellite").await;
            device.send("info", json!({})).await;
            ready.notified().await;
            settings.save(true, "Hall Echo").await;
            gateway.assert_upgrade(4).await;
            assert_installed(home.path(), &mock).await;

            missing_capture(&gateway, &mock, &mut device).await;
            stale.notified().await;
            mic_start(&gateway, &mock, &mut app, &mut device, &stale).await;
            text_start(&gateway, &mock, &mut app, &mut device).await;
            wake_and_end(&gateway, &mock, &mut app, &mut device).await;
            assert_eq!(receipts(&mock).len(), 6);
            settings.close().await;
            gateway.assert_disconnect().await;
            grant.close().await;
        })))
        .await;
}

async fn missing_capture(gateway: &Gateway, mock: &Mock, device: &mut Device) {
    let at = Instant::now();
    send(
        gateway,
        &command("start-session", "no-capture", ROUTE, &json!({"reason":"fixture"})),
    )
    .await;
    let id = device.remote_request().await;
    assert_ne!(id, "no-capture");
    advance(Duration::from_millis(1999)).await;
    assert!(receipts(mock).is_empty());
    advance(Duration::from_millis(1)).await;
    let reply = outcome(mock, "no-capture").await;
    assert_eq!(reply["failure"]["code"], "NOT_READY");
    assert!((Duration::from_secs(2)..=Duration::from_millis(2500)).contains(&at.elapsed()));
    device.pipeline(Some(&id)).await;
}

async fn mic_start(
    gateway: &Gateway,
    mock: &Mock,
    app: &mut BufReader<DuplexStream>,
    device: &mut Device,
    ordered: &Notify,
) {
    let at = Instant::now();
    send(
        gateway,
        &command(
            "start-session",
            "mic",
            ROUTE,
            &json!({"reason":"fixture","listen":false}),
        ),
    )
    .await;
    let id = device.remote_request().await;
    device.pipeline(Some(&id)).await;
    device.send("hassmic-mute", json!({"muted":false})).await;
    ordered.notified().await;
    advance(Duration::from_millis(1999)).await;
    assert_eq!(receipts(mock).len(), 1);
    advance(Duration::from_millis(1)).await;
    let reply = outcome(mock, "mic").await;
    assert_eq!(
        (reply["ok"].as_bool(), reply["state"].as_str()),
        (Some(true), Some("listening"))
    );
    assert!((Duration::from_secs(2)..=Duration::from_millis(2500)).contains(&at.elapsed()));
    finish_capture(app, device).await;
    assert_end(mock, reply["sessionId"].as_str().unwrap(), "remote", "completed").await;
}

async fn finish_capture(app: &mut BufReader<DuplexStream>, device: &mut Device) {
    app_start(app).await;
    play_reply(app, device).await;
    device.receive("hassmic-end").await;
    app_stop(app).await;
}

async fn text_start(gateway: &Gateway, mock: &Mock, app: &mut BufReader<DuplexStream>, device: &mut Device) {
    let at = Instant::now();
    send(
        gateway,
        &command(
            "start-session",
            "text",
            ROUTE,
            &json!({"reason":"fixture","initialText":"hello","listen":false}),
        ),
    )
    .await;
    app_start(app).await;
    let append = rpc(app, "thread/realtime/appendText").await;
    assert_eq!(
        append["params"],
        json!({"threadId":"test-thread","role":"user","text":"hello"})
    );
    answer(app, &append, json!({})).await;
    let reply = outcome(mock, "text").await;
    assert_eq!(reply["state"], "thinking");
    assert_eq!(reply["ok"], true);
    assert!(at.elapsed() <= Duration::from_secs(1));
    let session = reply["sessionId"].as_str().unwrap();
    play_reply(app, device).await;
    app_stop(app).await;
    assert_end(mock, session, "remote", "completed").await;
}

async fn play_reply(app: &mut BufReader<DuplexStream>, device: &mut Device) {
    device.receive("audio-start").await;
    write(app, &json!({"method":"thread/realtime/transcript/done","params":{"threadId":"test-thread","role":"assistant","text":"fixture reply"}})).await;
    device.receive("audio-stop").await;
    device.send("played", Value::Null).await;
}

async fn wake_and_end(gateway: &Gateway, mock: &Mock, app: &mut BufReader<DuplexStream>, device: &mut Device) {
    device.pipeline(None).await;
    app_start(app).await;
    device.receive("audio-start").await;
    advance(Duration::from_millis(250)).await;
    let wake = message(mock, "report-state", "origin", &json!("wake")).await;
    let id = wake["sessionId"].as_str().unwrap();
    assert_busy(gateway, mock, id).await;
    end_wake(gateway, mock, device, id).await;
    advance(Duration::from_secs(1)).await;
    app_stop(app).await;
}

async fn end_wake(gateway: &Gateway, mock: &Mock, device: &mut Device, id: &str) {
    let ended = Instant::now();
    send(gateway, &command("end-session", "end", ROUTE, &json!({"sessionId":id}))).await;
    device.receive("hassmic-flush").await;
    device.send("played", Value::Null).await;
    device.receive("hassmic-end").await;
    assert_eq!(
        outcome(mock, "end").await,
        json!({"ok":true,"state":"idle","ending":false})
    );
    assert!(ended.elapsed() <= Duration::from_secs(1));
    assert_end(mock, id, "wake", "remote").await;
    send(
        gateway,
        &command("end-session", "repeat", ROUTE, &json!({"sessionId":id})),
    )
    .await;
    assert_eq!(outcome(mock, "repeat").await["ok"], true);
}

async fn assert_busy(gateway: &Gateway, mock: &Mock, id: &str) {
    let started = Instant::now();
    send(
        gateway,
        &command(
            "start-session",
            "busy",
            ROUTE,
            &json!({"reason":"fixture","initialText":"busy"}),
        ),
    )
    .await;
    let busy = outcome(mock, "busy").await;
    assert_eq!(
        (
            busy["failure"]["code"].as_str(),
            busy["sessionId"].as_str(),
            busy["origin"].as_str()
        ),
        (Some("BUSY"), Some(id), Some("wake"))
    );
    assert!(started.elapsed() <= Duration::from_secs(1));
}

async fn assert_end(mock: &Mock, id: &str, origin: &str, reason: &str) {
    let payload = message(mock, "session-ended", "sessionId", &json!(id)).await;
    assert_eq!(
        (payload["origin"].as_str(), payload["reason"].as_str()),
        (Some(origin), Some(reason))
    );
    assert!(payload["endedAt"].as_str().unwrap().ends_with('Z'));
}

async fn outcome(mock: &Mock, id: &str) -> Value {
    loop {
        if let Some(reply) = receipts(mock)
            .into_iter()
            .filter_map(|receipt| receipt.json)
            .find(|body| body["payload"]["requestId"] == id)
        {
            assert_eq!(reply["target"]["value"], ROUTE);
            return reply["payload"]["outcome"].clone();
        }
        tokio::task::yield_now().await;
    }
}

async fn message(mock: &Mock, name: &str, field: &str, expected: &Value) -> Value {
    loop {
        if let Some(body) = mock
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter_map(|request| request.json.as_ref())
            .find(|body| body["header"]["name"] == name && body["payload"][field] == *expected)
        {
            assert_eq!(body["target"]["value"], ROUTE);
            return body["payload"].clone();
        }
        tokio::task::yield_now().await;
    }
}

async fn app_start(app: &mut BufReader<DuplexStream>) {
    let thread = rpc(app, "thread/start").await;
    answer(app, &thread, json!({"thread":{"id":"test-thread"}})).await;
    let start = rpc(app, "thread/realtime/start").await;
    write(
        app,
        &json!({"method":"thread/realtime/sdp","params":{"threadId":"test-thread","sdp":"fixture-answer"}}),
    )
    .await;
    answer(app, &start, json!({})).await;
}

async fn app_stop(app: &mut BufReader<DuplexStream>) {
    let stop = rpc(app, "thread/realtime/stop").await;
    write(
        app,
        &json!({"method":"thread/realtime/closed","params":{"threadId":"test-thread"}}),
    )
    .await;
    answer(app, &stop, json!({})).await;
}

async fn rpc(app: &mut BufReader<DuplexStream>, method: &str) -> Value {
    let mut line = String::new();
    app.read_line(&mut line).await.unwrap();
    let request: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(request["method"], method);
    request
}

async fn answer(app: &mut BufReader<DuplexStream>, request: &Value, result: Value) {
    write(app, &json!({"id":request["id"],"result":result})).await;
}

async fn write(app: &mut BufReader<DuplexStream>, data: &Value) {
    app.get_mut().write_all(format!("{data}\n").as_bytes()).await.unwrap();
}

struct Device {
    wire: TcpStream,
    decoder: Decoder,
    frames: std::collections::VecDeque<Event>,
}

impl Device {
    fn new(wire: TcpStream) -> Self {
        Self {
            wire,
            decoder: Decoder::new(),
            frames: std::collections::VecDeque::new(),
        }
    }

    async fn receive(&mut self, name: &str) -> Event {
        loop {
            if let Some(frame) = self.frames.pop_front() {
                if frame.header.r#type == name {
                    return frame;
                }
                if frame.header.r#type == "describe" {
                    self.send("info", json!({})).await;
                }
                continue;
            }
            let mut bytes = [0; 4096];
            let n = self.wire.read(&mut bytes).await.unwrap();
            assert_ne!(n, 0, "missing device frame {name}");
            self.frames.extend(self.decoder.feed(&bytes[..n]).unwrap());
        }
    }

    async fn send(&mut self, kind: &str, data: Value) {
        let frame = Event {
            header: Header {
                r#type: kind.into(),
                data: None,
                data_length: 0,
                payload_length: 0,
            },
            data: Some(data),
            payload: vec![],
        };
        self.wire.write_all(&encode(&frame)).await.unwrap();
    }

    async fn remote_request(&mut self) -> String {
        let request = self.receive("hassmic-start-run").await;
        request.header.data.or(request.data).unwrap()["requestId"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn pipeline(&mut self, id: Option<&str>) {
        let mut data = json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"Fixture"});
        if let Some(id) = id {
            data["requestId"] = json!(id);
        }
        self.send("run-pipeline", data).await;
    }
}
