//! T3: paired HTTP and app-server handoff through the prebuilt Linux bridge.
use super::*;
use reqwest::{Client, StatusCode, Url};
use tokio::net::{TcpListener, TcpStream};

const CODE: &str = "TEST-CODE";
const VERIFY: &str = "https://example.invalid/verify"; // lint-rules: allow-literal fake sign-in URI is displayed only
const EPOCH: &str = "1800000000";

struct Journey {
    fixture: Fixture,
    listener: TcpListener,
    process: Process,
    client: Client,
    screen: SocketAddr,
    trace: PathBuf,
}

impl Journey {
    async fn start(mode: &str, initially_linked: bool) -> Self {
        let fixture = Fixture::new(mode);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        restart::binary_home(&fixture, listener.local_addr().unwrap().port());
        if mode == "chatgpt" {
            std::fs::write(fixture.home.join("mode"), "screen-account").unwrap();
            std::fs::write(fixture.home.join("screen-recovery"), "").unwrap();
            if initially_linked {
                std::fs::write(fixture.home.join("auth.json"), "synthetic account").unwrap();
            }
        } else {
            std::fs::write(fixture.directory.path().join("credential"), "synthetic-nominated-key").unwrap();
        }
        let loopback = "http://127.0.0.1:1";
        let process = Process::spawn(super::super::voice_entry::command(&fixture).args([
            "--live-loopback",
            loopback,
            "--model-loopback",
            loopback,
            "--sim-clock",
            "--sim-epoch",
            EPOCH,
        ]));
        let trace = fixture.directory.path().join("trace");
        let screen = screen_address(&trace, 0).await;
        let client = Client::builder()
            .no_proxy()
            .timeout(OBSERVATION_TIMEOUT)
            .build()
            .unwrap();
        Self {
            fixture,
            listener,
            process,
            client,
            screen,
            trace,
        }
    }

    async fn ready(&mut self) -> TcpStream {
        restart::binary_ready(&self.listener, &mut self.process).await
    }

    async fn action(&self, action: &Value, token: Option<&str>, origin: Option<&str>) -> StatusCode {
        let mut url = Url::parse("http://127.0.0.1/actions").unwrap();
        url.set_port(Some(self.screen.port())).unwrap();
        if let Some(token) = token {
            url.query_pairs_mut().append_pair("token", token);
        }
        let mut request = self.client.post(url).json(action);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        request.send().await.unwrap().status()
    }

    async fn relink(&self) {
        assert_eq!(
            self.action(&json!({"action":"relink"}), Some("synthetic-screen-pair"), None)
                .await,
            204
        );
    }

    async fn health(&self) -> Value {
        let rows = trace(&self.trace);
        let address = rows[0]["health_addr"]
            .as_str()
            .or_else(|| rows.iter().find(|row| row["event"] == "bridge.started")?["health_addr"].as_str())
            .unwrap();
        let response = crate::health(address, "GET", "/healthz").await;
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    }

    fn methods(&self) -> Vec<String> {
        std::fs::read_to_string(self.fixture.home.join("methods"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn launches(&self) -> usize {
        std::fs::read_to_string(self.fixture.home.join("launches"))
            .unwrap()
            .lines()
            .count()
    }

    async fn finish(&mut self, code: i32) {
        self.process.finish(code).await;
        let logs = self.process.stderr().await;
        let trace = std::fs::read_to_string(&self.trace).unwrap();
        for private in [CODE, VERIFY, "synthetic private error"] {
            assert!(!logs.contains(private));
            assert!(!trace.contains(private));
        }
        super::super::host::released(&self.fixture).await;
    }
}

async fn state(events: &mut bridge_wire_screen::ScreenEvents, expected: &str) -> Value {
    loop {
        let frame = events.next(expected).await.unwrap().unwrap();
        if frame["state"]["state"] == expected {
            return frame["state"].clone();
        }
    }
}

async fn denied(journey: &Journey) {
    let mut url = Url::parse("http://127.0.0.1/events").unwrap();
    url.set_port(Some(journey.screen.port())).unwrap();
    let response = journey.client.get(url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.bytes().await.unwrap().is_empty());
    for (token, origin, status) in [
        (None, None, StatusCode::UNAUTHORIZED),
        (Some("wrong"), None, StatusCode::UNAUTHORIZED),
        (
            Some("synthetic-screen-pair"),
            Some("http://127.0.0.1:1"),
            StatusCode::FORBIDDEN,
        ),
    ] {
        assert_eq!(journey.action(&json!({"action":"relink"}), token, origin).await, status);
    }
    assert_eq!(journey.launches(), 1);
    assert_eq!(journey.methods(), ["initialize", "initialized", "account/read"]);
}

fn relink_state(message: &str, muted: bool) -> Value {
    json!({"state":"open_ai_relink","message":message,"starting":false,"muted":muted})
}

async fn challenge(journey: &Journey, events: &mut bridge_wire_screen::ScreenEvents, muted: bool, attempt: &str) {
    assert_eq!(
        state(events, "open_ai_signin").await,
        json!({"state":"open_ai_signin","attempt_id":attempt,"user_code":CODE,
            "verification_uri":VERIFY,"expires_at_unix":1_800_000_300_u64,"muted":muted})
    );
    let kind = std::fs::read_to_string(journey.fixture.home.join("login-type")).unwrap();
    assert_eq!(kind, "chatgptDeviceCode");
    assert_eq!(
        journey
            .methods()
            .iter()
            .filter(|method| *method == "account/login/start")
            .count(),
        1
    );
    assert_eq!(journey.health().await["relink_required"], true);
}

#[tokio::test]
#[ignore = "T3 uses a prebuilt Linux bridge, local Python, and paired IPv4 HTTP"]
async fn t3_bridge_screen_signin_paired_success_reuses_the_process() {
    let mut journey = Journey::start("chatgpt", false).await;
    let pid = journey.process.child.id().unwrap();
    let identity = restart::process_identity(pid);
    let mut events = bridge_wire_screen::ScreenEvents::connect(journey.screen.port()).await;
    assert_eq!(
        state(&mut events, "open_ai_relink").await,
        relink_state("Relink to sign in to ChatGPT.", false)
    );
    denied(&journey).await;
    journey.relink().await;
    challenge(&journey, &mut events, false, "attempt-1").await;
    journey.fixture.control().await.write_all(b"success").await.unwrap();
    let _satellite = journey.ready().await;
    while state(&mut events, "ready").await != json!({"state":"ready","message":"Echo Live ready"}) {}
    assert_eq!(journey.health().await["relink_required"], false);
    assert_eq!(journey.process.child.id(), Some(pid));
    assert_eq!(restart::process_identity(pid), identity);
    assert_eq!(journey.launches(), 3);
    let expected = ["account/read", "account/login/start", "account/read"];
    let account: Vec<_> = journey
        .methods()
        .into_iter()
        .filter(|method| method.starts_with("account/"))
        .collect();
    assert_eq!(account, expected);
    drop(events);
    journey.finish(0).await;
    writeln!(std::io::stdout().lock(),
        "Paired 204, denied 401/403, code={CODE}, URI={VERIFY}, expiry=1800000300; three reaped generations, {expected:?}, same bridge PID {pid}, ready=true"
    ).unwrap();
}

#[tokio::test]
#[ignore = "T3 uses a prebuilt Linux bridge, local Python, and paired IPv4 HTTP"]
async fn t3_bridge_screen_signin_rejection_keeps_health_latched() {
    for (outcome, message, launches) in [
        (
            b"rejected".as_slice(),
            "ChatGPT sign-in was rejected. Relink to try again.",
            2,
        ),
        (
            b"success-no-account".as_slice(),
            "ChatGPT still needs sign-in. Relink to try again.",
            3,
        ),
    ] {
        let mut journey = Journey::start("chatgpt", false).await;
        let mut events = bridge_wire_screen::ScreenEvents::connect(journey.screen.port()).await;
        assert_eq!(
            state(&mut events, "open_ai_relink").await,
            relink_state("Relink to sign in to ChatGPT.", false)
        );
        journey.relink().await;
        challenge(&journey, &mut events, false, "attempt-1").await;
        journey.fixture.control().await.write_all(outcome).await.unwrap();
        loop {
            if state(&mut events, "open_ai_relink").await == relink_state(message, false) {
                break;
            }
        }
        assert_eq!(journey.health().await["relink_required"], true);
        assert_eq!(journey.launches(), launches);
        drop(events);
        journey.finish(1).await;
        let rows = trace(&journey.trace);
        assert_eq!(rows.iter().filter(|row| row["event"] == "bridge.ready").count(), 0);
        let event = if launches == 2 {
            "auth.signin_failed"
        } else {
            "auth.signin_completed"
        };
        let receipt = rows.iter().find(|row| row["event"] == event).unwrap();
        assert_eq!(receipt["reason"].as_str(), (launches == 2).then_some("authorization"));
        writeln!(
            std::io::stdout().lock(),
            "{event}: {message} health.relink_required=true launches={launches} ready=0"
        )
        .unwrap();
    }
}

#[tokio::test]
#[ignore = "T3 uses a prebuilt Linux bridge, local Python, and paired IPv4 HTTP"]
async fn t3_bridge_screen_signin_respects_mode_and_preserves_microphone_privacy() {
    for mode in ["chatgpt", "api_key"] {
        let mut journey = Journey::start(mode, true).await;
        let mut satellite = journey.ready().await;
        let mut events = bridge_wire_screen::ScreenEvents::connect(journey.screen.port()).await;
        let ready = json!({"state":"ready","message":"Echo Live ready"});
        assert_eq!(state(&mut events, "ready").await, ready);
        let methods = journey.methods();
        journey.relink().await;
        assert_eq!(state(&mut events, "ready").await, ready);
        assert_eq!(journey.methods(), methods);
        if mode == "api_key" {
            drop(events);
            journey.finish(0).await;
            continue;
        }
        bridge_short::send(&mut satellite, "hassmic-mute", json!({"muted":true})).await;
        assert_eq!(state(&mut events, "muted").await, json!({"state":"muted"}));
        let response = journey
            .client
            .post(settings_runtime_url(journey.screen, "/settings/v2/openai"))
            .json(&json!({"version":2,"operation":"unlink","confirm":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            state(&mut events, "open_ai_relink").await,
            relink_state("Relink to sign in to ChatGPT.", true)
        );
        journey.relink().await;
        challenge(&journey, &mut events, true, "attempt-2").await;
        assert_eq!(
            journey
                .action(
                    &json!({"action":"approve","id":"stale"}),
                    Some("synthetic-screen-pair"),
                    None
                )
                .await,
            204
        );
        journey.fixture.control().await.write_all(b"success").await.unwrap();
        assert_muted_readmission(&mut journey, &mut events, &ready).await;
        drop(events);
        journey.finish(0).await;
    }
    writeln!(
        std::io::stdout().lock(),
        "Healthy ChatGPT and API-key Relink add zero RPCs; paired ChatGPT challenge and recovered screen retain mute"
    )
    .unwrap();
}

async fn assert_muted_readmission(journey: &mut Journey, events: &mut bridge_wire_screen::ScreenEvents, ready: &Value) {
    let mut recovered = journey.ready().await;
    assert_eq!(state(events, "muted").await, json!({"state":"muted"}));
    assert_eq!(journey.health().await["relink_required"], false);
    bridge_short::send(
        &mut recovered,
        "run-pipeline",
        json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"synthetic"}),
    )
    .await;
    bridge_short::send(&mut recovered, "hassmic-mute", json!({"muted":false})).await;
    assert_eq!(&state(events, "ready").await, ready);
    assert_eq!(
        trace(&journey.trace)
            .iter()
            .filter(|row| row["event"] == "run.started")
            .count(),
        0
    );
}
