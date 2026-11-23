//! Configured binary witness for the shared paired and operator account slot.
use super::*;
use echo_mcp_auth::mock::Mock;

pub(super) const NEXT_CODE: &str = "NEXT-CODE";
pub(super) const NEXT_VERIFY: &str = "https://example.invalid/renew"; // lint-rules: allow-literal fake display only

const START: &str = "openai_signin_start";
const STATUS: &str = "openai_signin_status";
const EXPIRED: &str = "ChatGPT sign-in code expired. Relink for a new code.";
const CANCELLED: &str = "ChatGPT sign-in cancelled. Relink for a new code.";

impl Journey {
    async fn operator_url(&self) -> Url {
        let port = tokio::time::timeout(OBSERVATION_TIMEOUT, async {
            loop {
                if let Some(row) = trace(&self.trace)
                    .iter()
                    .find(|row| row["event"] == "mcp.operator.listening")
                {
                    let address: SocketAddr = row["address"].as_str().unwrap().parse().unwrap();
                    assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
                    break address.port();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut url = Url::parse("http://127.0.0.1/operator/mcp").unwrap();
        url.set_port(Some(port)).unwrap();
        url
    }

    async fn operator(&self, operation: &str, expected: u16) -> Value {
        let receipt = self
            .operator_request(operation)
            .await
            .bearer_auth("synthetic-screen-pair")
            .send()
            .await
            .unwrap();
        assert_eq!(receipt.status(), expected);
        assert_eq!(receipt.headers()["cache-control"], "no-store");
        let bytes = receipt.bytes().await.unwrap();
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        }
    }

    async fn operator_request(&self, operation: &str) -> reqwest::RequestBuilder {
        self.client
            .post(self.operator_url().await)
            .json(&json!({"operation":operation}))
    }

    async fn screen_action(&self, action: &str, attempt: &str) {
        assert_eq!(
            self.action(
                &json!({"action":action,"attempt_id":attempt}),
                Some("synthetic-screen-pair"),
                None
            )
            .await,
            204
        );
    }

    async fn advance(&mut self, milliseconds: u64) {
        let receipt = self
            .process
            .command(&json!({"action":"advance_clock","milliseconds":milliseconds}).to_string())
            .await;
        assert_private(&[&receipt]);
        assert_eq!(
            serde_json::from_str::<Value>(&receipt).unwrap(),
            json!({
                "event":"bridge.clock_advanced", "milliseconds":milliseconds
            })
        );
    }

    fn login_count(&self) -> usize {
        self.methods()
            .iter()
            .filter(|method| *method == "account/login/start")
            .count()
    }

    async fn events(&self) -> bridge_wire_screen::ScreenEvents {
        bridge_wire_screen::ScreenEvents::connect(self.screen.port()).await
    }
}

async fn expect_challenge(journey: &Journey, events: &mut bridge_wire_screen::ScreenEvents, number: u64, expiry: u64) {
    let (code, uri) = if number == 1 {
        (CODE, VERIFY)
    } else {
        (NEXT_CODE, NEXT_VERIFY)
    };
    assert_eq!(
        state(events, "open_ai_signin").await,
        json!({
            "state":"open_ai_signin","attempt_id":format!("attempt-{number}"),"user_code":code,
            "verification_uri":uri,"expires_at_unix":expiry,"muted":false
        })
    );
    assert_eq!(
        journey.operator(STATUS, 200).await,
        json!({
            "state":"pending","user_code":code,"verification_uri":uri,"expires_at_unix":expiry
        })
    );
    assert_private(&[&journey.health().await.to_string()]);
}

async fn terminal(events: &mut bridge_wire_screen::ScreenEvents, message: &str) {
    let expected = relink_state(message, false);
    while state(events, "open_ai_relink").await != expected {}
}

async fn deny_operator(journey: &Journey) {
    for operation in [START, STATUS] {
        let bad = journey
            .operator_request(operation)
            .await
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), 401);
        assert!(bad.bytes().await.unwrap().is_empty());
        let origin = journey
            .operator_request(operation)
            .await
            .bearer_auth("synthetic-screen-pair")
            .header("origin", "http://127.0.0.1")
            .send()
            .await
            .unwrap();
        assert_eq!(origin.status(), 403);
        assert!(origin.bytes().await.unwrap().is_empty());
    }
}

#[tokio::test]
#[ignore = "T3 uses a prebuilt Linux bridge, local Python and IPv4 operator HTTP"]
async fn t3_bridge_screen_signin_operator_starts_and_readmits() {
    let mock = Mock::bind().await.unwrap();
    let mut journey = Journey::configured("chatgpt", false, Some(&mock.base)).await;
    let mut events = journey.events().await;
    terminal(&mut events, "Relink to sign in to ChatGPT.").await;
    deny_operator(&journey).await;
    assert_eq!(journey.login_count(), 0);
    assert_eq!(journey.operator(STATUS, 200).await, json!({"state":"relink_required"}));
    let pid = journey.process.child.id().unwrap();
    let identity = restart::process_identity(pid);
    assert_eq!(journey.operator(START, 202).await, json!({"outcome":"accepted"}));
    expect_challenge(&journey, &mut events, 1, 1_800_000_300).await;
    deny_operator(&journey).await;
    assert_eq!(journey.operator(START, 409).await, Value::Null);
    assert_eq!(journey.login_count(), 1);
    journey.fixture.control().await.write_all(b"success").await.unwrap();
    let _satellite = journey.ready().await;
    assert_eq!(journey.process.child.id(), Some(pid));
    assert_eq!(restart::process_identity(pid), identity);
    assert_eq!(journey.health().await["relink_required"], false);
    assert_eq!(journey.operator(STATUS, 200).await, json!({"state":"ready"}));
    assert_eq!(journey.operator(START, 409).await, Value::Null);
    assert_eq!(journey.login_count(), 1);
    drop(events);
    journey.finish(0).await;
    assert_trace(
        &journey,
        &[("auth.signin_started", None), ("auth.signin_completed", None)],
    );
    report(
        "Operator 202, denied 401/403, JSON no-store and exact paired parity; one RPC, same PID ready, healthy start 409",
    );
}

#[tokio::test]
#[ignore = "T3 uses a prebuilt Linux bridge, local Python and IPv4 operator HTTP"]
async fn t3_bridge_screen_signin_operator_expiry_renew_cancel_and_shutdown() {
    let mock = Mock::bind().await.unwrap();
    let mut journey = Journey::configured("chatgpt", false, Some(&mock.base)).await;
    let mut events = journey.events().await;
    terminal(&mut events, "Relink to sign in to ChatGPT.").await;
    compete(&journey).await;
    expect_challenge(&journey, &mut events, 1, 1_800_000_300).await;
    assert_eq!(journey.login_count(), 1);
    journey.advance(60_000).await;
    journey.screen_action("renew_open_ai_signin", "attempt-1").await;
    expect_challenge(&journey, &mut events, 2, 1_800_000_360).await;
    assert_eq!(journey.launches(), 3);
    journey.screen_action("cancel_open_ai_signin", "attempt-1").await;
    expect_challenge(&journey, &mut journey.events().await, 2, 1_800_000_360).await;
    journey.advance(300_000).await;
    terminal(&mut events, EXPIRED).await;
    assert_eq!(journey.operator(STATUS, 200).await, json!({"state":"relink_required"}));
    journey.screen_action("renew_open_ai_signin", "attempt-2").await;
    assert_eq!(journey.login_count(), 2);
    assert_eq!(journey.operator(START, 202).await, json!({"outcome":"accepted"}));
    expect_challenge(&journey, &mut events, 3, 1_800_000_660).await;
    let child = std::fs::read_to_string(journey.fixture.home.join("peer-pid")).unwrap();
    journey.screen_action("cancel_open_ai_signin", "attempt-3").await;
    terminal(&mut events, CANCELLED).await;
    assert!(!Path::new(&format!("/proc/{child}")).exists());
    assert_eq!(
        state(&mut journey.events().await, "open_ai_relink").await,
        relink_state(CANCELLED, false)
    );
    assert_eq!(journey.operator(STATUS, 200).await, json!({"state":"relink_required"}));
    assert_eq!(journey.health().await["relink_required"], true);
    shutdown_and_restart(journey, events).await;
    report(
        "One competing start; renewed code and expiry match; stale cancel ignored; 300000 ms expiry; retry 202; cancel reaped and reconnect clean; shutdown and fresh launch clean; exact code and URL matches in trace/stdout/stderr=0",
    );
}

async fn compete(journey: &Journey) {
    let call = journey
        .operator_request(START)
        .await
        .bearer_auth("synthetic-screen-pair");
    let settings = journey
        .client
        .post(settings_runtime_url(journey.screen, "/settings/v2/openai"))
        .json(&json!({"version":2,"operation":"link"}));
    let (operator, (), settings) = tokio::join!(call.send(), journey.relink(), settings.send());
    let states = [operator.unwrap().status(), settings.unwrap().status()];
    assert!(states.iter().all(|status| matches!(status.as_u16(), 202 | 409)));
    assert_ne!(states, [StatusCode::ACCEPTED; 2]);
}

async fn shutdown_and_restart(mut journey: Journey, mut events: bridge_wire_screen::ScreenEvents) {
    journey.relink().await;
    expect_challenge(&journey, &mut events, 4, 1_800_000_660).await;
    let operator = journey.operator_url().await;
    drop(events);
    journey.finish(1).await;
    assert_trace(
        &journey,
        &[
            ("auth.signin_started", None),
            ("auth.signin_failed", Some("cancelled")),
            ("auth.signin_started", None),
            ("auth.signin_expired", None),
            ("auth.signin_started", None),
            ("auth.signin_failed", Some("cancelled")),
            ("auth.signin_started", None),
            ("auth.signin_failed", Some("cancelled")),
        ],
    );
    assert!(TcpStream::connect(("127.0.0.1", journey.screen.port())).await.is_err());
    assert!(
        TcpStream::connect(("127.0.0.1", operator.port().unwrap()))
            .await
            .is_err()
    );
    let login_count = journey.login_count();
    journey.process = Process::spawn(super::super::super::voice_entry::command(&journey.fixture).args([
        "--live-loopback",
        "http://127.0.0.1:1",
        "--model-loopback",
        "http://127.0.0.1:1",
        "--sim-clock",
        "--sim-epoch",
        EPOCH,
    ]));
    journey.screen = screen_address(&journey.trace, 1).await;
    let mut reconnected = journey.events().await;
    terminal(&mut reconnected, "Relink to sign in to ChatGPT.").await;
    assert_eq!(journey.login_count(), login_count);
    drop(reconnected);
    journey.finish(1).await;
}

#[tokio::test]
#[ignore = "T3 uses a prebuilt Linux bridge, local Python and IPv4 operator HTTP"]
async fn t3_bridge_screen_signin_operator_refuses_healthy_and_api_key_modes() {
    let mock = Mock::bind().await.unwrap();
    for (mode, expected, login_count) in [("chatgpt", "ready", 0), ("api_key", "unavailable", 1)] {
        let mut journey = Journey::configured(mode, true, Some(&mock.base)).await;
        let _satellite = journey.ready().await;
        assert_eq!(journey.operator(STATUS, 200).await, json!({"state":expected}));
        assert_eq!(journey.operator(START, 409).await, Value::Null);
        assert_eq!(journey.login_count(), login_count);
        journey.finish(0).await;
        assert_trace(&journey, &[]);
    }
    report(
        "Ready ChatGPT with Port OS unlinked and API-key mode both return 409; neither starts ChatGPT device-code sign-in",
    );
}

fn assert_trace(journey: &Journey, expected: &[(&str, Option<&str>)]) {
    let rows = trace(&journey.trace);
    let auth: Vec<_> = rows
        .iter()
        .filter_map(|row| {
            let event = row["event"].as_str()?;
            event
                .starts_with("auth.signin_")
                .then(|| (event, row["reason"].as_str()))
        })
        .collect();
    assert_eq!(auth, expected);
}

fn report(message: &str) {
    writeln!(std::io::stdout().lock(), "{message}").unwrap();
}
