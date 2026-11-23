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
            let mut gateway = Gateway::open(&mock).await;
            let Fixture {
                settings,
                mut device,
                mut app,
                stale,
            } = fixture(home.path(), &mock, &gateway, grant.grant.clone()).await;
            settings.save(true, "Hall Echo").await;
            gateway.assert_upgrade(CLAIMED).await;
            assert_installed(home.path(), &mock).await;

            missing_capture(&gateway, &mock, &mut device).await;
            stale.notified().await;
            mic_start(&gateway, &mock, &mut app, &mut device, &stale).await;
            text_start(&gateway, &mock, &mut app, &mut device).await;
            text_followup(&gateway, &mock, &mut app, &mut device, &stale).await;
            wake_and_end(&gateway, &mock, &mut app, &mut device).await;
            safety_refusals(&gateway, &mock, &mut device, &stale, home.path()).await;
            assert_eq!(receipts(&mock).len(), 9);
            settings.close().await;
            gateway.assert_disconnect().await;
            grant.close().await;
        })))
        .await;
}

pub(super) struct Fixture {
    pub(super) settings: Settings,
    pub(super) device: Device,
    pub(super) app: BufReader<DuplexStream>,
    pub(super) stale: Arc<Notify>,
}

pub(super) async fn fixture(home: &Path, mock: &Mock, gateway: &Gateway, shared_grant: Arc<Grant>) -> Fixture {
    fixture_with_camera(home, mock, gateway, shared_grant, None).await
}

pub(super) async fn fixture_with_camera(
    home: &Path,
    mock: &Mock,
    gateway: &Gateway,
    shared_grant: Arc<Grant>,
    camera: Option<Arc<dyn snapshot::CameraSource>>,
) -> Fixture {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let (connector, _) = crate::bridge_world::connector(listener.local_addr().unwrap().into());
    let (settings, app, ready, stale) =
        fixture_with_link(home, mock, gateway, shared_grant, connector, camera, true).await;
    let (device, _) = listener.accept().await.unwrap();
    let mut device = Device::new(device);
    device.receive("run-satellite").await;
    device.send("info", json!({})).await;
    ready.notified().await;
    Fixture {
        settings,
        device,
        app,
        stale,
    }
}

pub(super) async fn fixture_with_link(
    home: &Path,
    mock: &Mock,
    gateway: &Gateway,
    shared_grant: Arc<Grant>,
    connector: impl echo_link::LinkConnector + 'static,
    camera: Option<Arc<dyn snapshot::CameraSource>>,
    remote_mic: bool,
) -> (Settings, BufReader<DuplexStream>, Arc<Notify>, Arc<Notify>) {
    let (service, send_settings) = Settings::with_camera(home, mock, gateway, false, camera);
    let (app, server) = tokio::io::duplex(16384);
    let ready = Arc::new(Notify::new());
    let signal = ready.clone();
    let stale = Arc::new(Notify::new());
    let ordered = stale.clone();
    let fixture_home = home.to_path_buf();
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
                    (&fixture_home, remote_mic),
                )
                .await;
                assert_eq!(result, Err(crate::Error::Control));
            };
            Box::pin(service.scope_with_grant(work, || shared_grant.link_status(), || Some(shared_grant.clone())))
                .await;
        })
    })
    .await;
    (settings, BufReader::new(server), ready, stale)
}

pub(super) async fn missing_capture(gateway: &Gateway, mock: &Mock, device: &mut Device) {
    let at = Instant::now();
    send(
        gateway,
        &command("start-session", "no-capture", DEVICE, &json!({"reason":"fixture"})),
    )
    .await;
    let id = device.remote_request().await;
    assert_ne!(id, "no-capture");
    advance(Duration::from_millis(1999)).await;
    assert!(receipts(mock).is_empty());
    advance(Duration::from_millis(1)).await;
    let reply = outcome(mock, "no-capture").await;
    assert_eq!(reply, json!({"error":{"code":"NOT_READY"}}));
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
            DEVICE,
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
    let reply = outcome(mock, "mic").await["startSession"].clone();
    assert_eq!(reply["state"], "LISTENING");
    assert!((Duration::from_secs(2)..=Duration::from_millis(2500)).contains(&at.elapsed()));
    finish_capture(app, device).await;
    assert_end(mock, reply["sessionId"].as_str().unwrap(), "REMOTE", "COMPLETED").await;
}

async fn finish_capture(app: &mut BufReader<DuplexStream>, device: &mut Device) {
    app_start(app).await;
    play_reply(app, device, "fixture reply").await;
    device.receive("hassmic-end").await;
    app_stop(app).await;
}

async fn text_start(gateway: &Gateway, mock: &Mock, app: &mut BufReader<DuplexStream>, device: &mut Device) {
    let reply = begin_text(gateway, mock, app, "text", false, "hello").await;
    let session = reply["sessionId"].as_str().unwrap();
    play_reply(app, device, "fixture reply").await;
    app_stop(app).await;
    assert_end(mock, session, "REMOTE", "COMPLETED").await;
}

async fn text_followup(
    gateway: &Gateway,
    mock: &Mock,
    app: &mut BufReader<DuplexStream>,
    device: &mut Device,
    ordered: &Notify,
) {
    let reply = begin_text(gateway, mock, app, "text-followup", true, "hello").await;
    play_reply(app, device, "fixture reply").await;
    let capture = device.remote_request().await;
    device.pipeline(Some(&capture)).await;
    device.send("hassmic-mute", json!({"muted":false})).await;
    ordered.notified().await;
    advance(Duration::from_secs(8)).await;
    device.receive("hassmic-end").await;
    app_stop(app).await;
    assert_end(mock, reply["sessionId"].as_str().unwrap(), "REMOTE", "COMPLETED").await;
}

pub(super) async fn begin_text(
    gateway: &Gateway,
    mock: &Mock,
    app: &mut BufReader<DuplexStream>,
    request: &str,
    listen: bool,
    text: &str,
) -> Value {
    let at = Instant::now();
    send(
        gateway,
        &command(
            "start-session",
            request,
            DEVICE,
            &json!({"reason":"fixture","initialText":text,"listen":listen}),
        ),
    )
    .await;
    app_start(app).await;
    let append = rpc(app, "thread/realtime/appendText").await;
    assert_eq!(
        append["params"],
        json!({"threadId":"test-thread","role":"user","text":text})
    );
    answer(app, &append, json!({})).await;
    let reply = outcome(mock, request).await["startSession"].clone();
    assert_eq!(reply["state"], "THINKING");
    assert!(at.elapsed() <= Duration::from_secs(1));
    reply
}

async fn safety_refusals(gateway: &Gateway, mock: &Mock, device: &mut Device, ordered: &Notify, home: &Path) {
    device.send("hassmic-mute", json!({"muted":true})).await;
    ordered.notified().await;
    assert_start_refused(gateway, mock, "mute", "MUTED").await;
    device.send("hassmic-mute", json!({"muted":false})).await;
    ordered.notified().await;
    let flag = home.join("voice-disabled");
    fs::write(&flag, "").unwrap();
    assert_start_refused(gateway, mock, "disable", "DISABLED").await;
}

async fn assert_start_refused(gateway: &Gateway, mock: &Mock, id: &str, code: &str) {
    let at = Instant::now();
    send(
        gateway,
        &command(
            "start-session",
            id,
            DEVICE,
            &json!({"reason":"fixture","initialText":"hello"}),
        ),
    )
    .await;
    assert_eq!(outcome(mock, id).await, json!({"error":{"code":code}}));
    assert!(at.elapsed() <= Duration::from_secs(1));
}

pub(super) async fn play_reply(app: &mut BufReader<DuplexStream>, device: &mut Device, text: &str) {
    device.receive("audio-start").await;
    write(app, &json!({"method":"thread/realtime/transcript/done","params":{"threadId":"test-thread","role":"assistant","text":text}})).await;
    device.receive("audio-stop").await;
    device.send("played", Value::Null).await;
}

async fn wake_and_end(gateway: &Gateway, mock: &Mock, app: &mut BufReader<DuplexStream>, device: &mut Device) {
    device.pipeline(None).await;
    app_start(app).await;
    device.receive("audio-start").await;
    start_delegation(app, device).await;
    advance(Duration::from_millis(250)).await;
    let wake = message(mock, "/assistant/origin", &json!("WAKE")).await;
    let id = wake["assistant"]["sessionId"].as_str().unwrap();
    assert_busy(gateway, mock, id).await;
    end_wake(gateway, mock, app, device, id).await;
    advance(Duration::from_secs(1)).await;
    app_stop(app).await;
}

async fn start_delegation(app: &mut BufReader<DuplexStream>, device: &mut Device) {
    write(
        app,
        &json!({"method":"turn/started","params":{"threadId":"test-thread","turn":{"id":"delegated-turn"}}}),
    )
    .await;
    let event = device.receive("hassmic-thinking").await;
    assert_eq!(event.header.data.or(event.data).unwrap(), json!({"thinking":true}));
}

async fn end_wake(gateway: &Gateway, mock: &Mock, app: &mut BufReader<DuplexStream>, device: &mut Device, id: &str) {
    let ended = Instant::now();
    send(
        gateway,
        &command("end-session", "end", DEVICE, &json!({"sessionId":id})),
    )
    .await;
    device.receive("hassmic-flush").await;
    device.send("played", Value::Null).await;
    device.receive("hassmic-end").await;
    let interrupt = rpc(app, "turn/interrupt").await;
    assert_eq!(
        interrupt["params"],
        json!({"threadId":"test-thread","turnId":"delegated-turn"})
    );
    answer(app, &interrupt, json!({})).await;
    assert_eq!(outcome(mock, "end").await, json!({"endSession":{"state":"IDLE"}}));
    assert!(ended.elapsed() <= Duration::from_secs(1));
    assert_end(mock, id, "WAKE", "REMOTE").await;
    send(
        gateway,
        &command("end-session", "repeat", DEVICE, &json!({"sessionId":id})),
    )
    .await;
    assert_eq!(outcome(mock, "repeat").await, json!({"endSession":{"state":"IDLE"}}));
}

async fn assert_busy(gateway: &Gateway, mock: &Mock, id: &str) {
    let started = Instant::now();
    send(
        gateway,
        &command(
            "start-session",
            "busy",
            DEVICE,
            &json!({"reason":"fixture","initialText":"busy"}),
        ),
    )
    .await;
    assert_eq!(
        outcome(mock, "busy").await,
        json!({"error":{"code":"BUSY","sessionId":id,"origin":"WAKE"}})
    );
    assert!(started.elapsed() <= Duration::from_secs(1));
}

pub(super) async fn assert_end(mock: &Mock, id: &str, origin: &str, reason: &str) {
    let payload = message(mock, "/sessionEnded/sessionId", &json!(id)).await["sessionEnded"].clone();
    assert_eq!(
        (payload["origin"].as_str(), payload["reason"].as_str()),
        (Some(origin), Some(reason))
    );
    assert!(payload["endedAt"].as_str().unwrap().ends_with('Z'));
}

/// The reply part of the one report answering `id`, without its `requestId`.
pub(super) async fn outcome(mock: &Mock, id: &str) -> Value {
    loop {
        if let Some(reply) = receipts(mock)
            .into_iter()
            .filter_map(|receipt| receipt.json)
            .find(|body| body["payload"]["requestId"] == id)
        {
            assert_eq!(reply["target"]["value"], DEVICE);
            assert_no_refusals(mock);
            let mut payload = reply["payload"].clone();
            let fields = payload.as_object_mut().unwrap();
            fields.remove("requestId");
            fields.remove("sampledAtMs");
            return payload;
        }
        tokio::task::yield_now().await;
    }
}

/// The first report payload whose value at `pointer` is `expected`.
pub(super) async fn message(mock: &Mock, pointer: &str, expected: &Value) -> Value {
    loop {
        if let Some(body) = mock
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.path == "/messages")
            .filter_map(|request| request.json.as_ref())
            .find(|body| body["payload"].pointer(pointer) == Some(expected))
        {
            assert_eq!(body["header"]["name"], "report-state");
            assert_eq!(body["target"]["value"], DEVICE);
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

pub(super) async fn app_stop(app: &mut BufReader<DuplexStream>) {
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

pub(super) struct Device {
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
            let frame = self.next().await;
            if frame.header.r#type == name {
                return frame;
            }
            if frame.header.r#type == "describe" {
                self.send("info", json!({})).await;
            }
        }
    }

    pub(super) async fn next(&mut self) -> Event {
        loop {
            if let Some(frame) = self.frames.pop_front() {
                return frame;
            }
            let mut bytes = [0; 4096];
            let n = self.wire.read(&mut bytes).await.unwrap();
            assert_ne!(n, 0, "missing device frame");
            self.frames.extend(self.decoder.feed(&bytes[..n]).unwrap());
        }
    }

    pub(super) async fn send(&mut self, kind: &str, data: Value) {
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
