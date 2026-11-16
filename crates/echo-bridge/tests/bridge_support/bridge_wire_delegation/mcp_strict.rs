//! T3: the delivered bridge owns the grant; section-9 peers enforce the wire.
//! Controlled Wyoming is a component boundary, not the pinned-host M5 scenario.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use echo_lab::lab_mcp::{Approval, Counts, Fixture, LinkCounts, Observations};
use echo_spoof_mcp::BearerValidator;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
struct Durable {
    admission: Arc<dyn BearerValidator>,
    directory: PathBuf,
    committed: Arc<Mutex<Vec<bool>>>,
}
impl BearerValidator for Durable {
    fn protected_resource(&self) -> Option<Value> {
        self.admission.protected_resource()
    }
    fn accepts(&self, bearer: &str) -> bool {
        let stored = record(&self.directory);
        let committed = stored["grant"]["access_token"].as_str() == Some(bearer);
        self.committed.lock().unwrap().push(committed);
        committed && self.admission.accepts(bearer)
    }
}
struct Harness {
    directory: tempfile::TempDir,
    fixture: Fixture,
    spoof: Spoof,
    live: bridge_peer::Live,
    model: Model,
    epoch: u64,
    committed: Arc<Mutex<Vec<bool>>>,
}
impl Harness {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let epoch = 1_800_000_000;
        let fixture = Fixture::bind(epoch).await.unwrap();
        let committed = Arc::new(Mutex::new(Vec::new()));
        let validator = Arc::new(Durable {
            admission: fixture.validator(),
            directory: directory.path().into(),
            committed: committed.clone(),
        });
        let spoof = Spoof::start_authorized(Options::default(), Some(validator))
            .await
            .unwrap();
        fixture.set_resource(spoof.url()).unwrap();
        let live = bridge_peer::Live::bind_frames(0).await;
        let model = failures::scripted(vec![
            tool("m5-first"),
            failures::answer("m5-linked"),
            tool("m5-replayed"),
        ])
        .await;
        Self {
            directory,
            fixture,
            spoof,
            live,
            model,
            epoch,
            committed,
        }
    }
    async fn start(&self, first: bool) -> Session {
        Session::start_resource(
            self.directory.path(),
            self.spoof.url().strip_suffix("/mcp").unwrap(),
            first,
            Some((&self.live.url, &self.model.origin)),
            Some(self.epoch),
        )
        .await
    }
    async fn link(&self, session: &mut Session) -> Value {
        let pending = operator(self.directory.path(), "link");
        let state = bridge_wire_screen::state(&mut session.screen).await;
        assert_eq!(state["state"], "link_code");
        assert_eq!(state["user_code"], "TEST-CODE");
        assert_eq!(state["verification_uri"], format!("{}/approve", self.fixture.base()));
        assert_eq!(state["expires_at_unix"], self.epoch + 1800);
        assert_eq!(self.spoof.snapshot().unwrap().requests, vec![]);
        assert_eq!(bridge_short::read(&mut session.satellite).await.0["type"], "describe");
        session.answer_probe().await;
        self.fixture.approve(Approval::Approved).unwrap();
        self.fixture.advance(5).unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        session.advance(5000).await;
        completed(&pending.await.unwrap(), 0, "MCP linked\n", "");
        assert_eq!(
            bridge_wire_screen::state(&mut session.screen).await,
            json!({"state":"ready","message":"Echo Live ready"})
        );
        record(self.directory.path())
    }
    async fn rotate(&self, initial: &Value, generation: u64) -> Value {
        completed(
            &operator(self.directory.path(), "--refresh-now").await.unwrap(),
            0,
            "MCP refreshed\n",
            "",
        );
        let stored = record(self.directory.path());
        assert_eq!(
            stored["grant"]["refresh_token"],
            format!("synthetic-refresh-1-{generation}")
        );
        assert_eq!(stored["private_key_jwk"], initial["private_key_jwk"]);
        assert_eq!(self.spoof.snapshot().unwrap().requests, vec![]);
        stored
    }
    fn path(&self) -> PathBuf {
        self.directory.path().join("trace")
    }
    fn assert_rotations(&self) {
        assert_eq!(
            self.fixture.counts().unwrap(),
            Counts {
                links: 1,
                polls: 1,
                refreshes: 2,
                ..Counts::default()
            }
        );
    }
    async fn close(self) {
        let mut spoof = bridge_spine_edges::Owner::new("strict MCP spoof", self.spoof.shutdown());
        let mut auth = bridge_spine_edges::Owner::new("strict MCP auth", self.fixture.shutdown());
        drop(self.live);
        spoof.stop();
        auth.stop();
        spoof.assert_closed().await;
        auth.assert_closed().await;
    }
}
#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_strict_signed_tools_after_crash_restart_and_refresh_replay() {
    drive(async {
        let mut h = Harness::new().await;
        let mut session = h.start(true).await;
        let initial = h.link(&mut session).await;
        h.rotate(&initial, 2).await;
        let mut observations = Observations::default();
        let first_process = observe_trace(&mut observations, &h.path(), 0);
        // Completed CLI is the durable-rotation barrier, before any resource use.
        session.process.child.kill().await.unwrap();
        assert!(!session.process.child.wait().await.unwrap().success());
        drop(session);
        observations.restarted();
        let mut restarted = h.start(false).await;
        restarted.restart_probe().await;
        restarted.advance(5000).await;
        h.rotate(&initial, 3).await;
        h.assert_rotations();
        wake_connected(&mut restarted).await;
        let (_next_wake, wakes) = tokio::sync::mpsc::channel(1);
        let satellite = tokio::spawn(journey::answer(restarted.satellite, wakes));
        h.live.events.send(event("m5-linked", "client")).await.unwrap();
        journey::reach(&mut restarted.process, &h.path(), "session.commentary.append")
            .await
            .unwrap();
        assert_success(&h);
        assert_commentary(&mut h).await;
        replay(&h, &initial).await;
        // A distinct delegated task on the retained Live call must reach MCP.
        h.live.events.send(event("m5-replayed", "client")).await.unwrap();
        journey::reach(&mut restarted.process, &h.path(), "mcp.relink_required")
            .await
            .unwrap();
        observations.screen(&assert_relink(&mut restarted.screen).await);
        journey::reach(&mut restarted.process, &h.path(), "mcp.run.failed")
            .await
            .unwrap();
        completed(
            &operator(h.directory.path(), "--refresh-now").await.unwrap(),
            1,
            "",
            "echo-bridge: Grant requires relinking\n",
        );
        completed(
            &operator(h.directory.path(), "unlink").await.unwrap(),
            0,
            "MCP unlinked\n",
            "",
        );
        assert_eq!(record(h.directory.path())["grant"], Value::Null);
        restarted.process.finish(0).await;
        satellite.await.unwrap();
        assert_no_restart_disconnect(&h.path());
        assert_replayed(&h);
        assert_measurements(&h, &mut observations, first_process);
        h.close().await;
    })
    .await;
}
async fn wake_connected(session: &mut Session) {
    journey::wake(&mut session.satellite).await;
    bridge_wire_screen::listening_then_connected(&mut session.screen).await;
}
async fn assert_commentary(h: &mut Harness) {
    assert_eq!(
        serde_json::from_str::<Value>(&h.live.commentary.recv().await.unwrap()).unwrap(),
        json!({"type":"session.commentary.append","delegation_id":"m5-linked","text":"m5-linked"})
    );
}
fn assert_success(h: &Harness) {
    let snapshot = h.spoof.snapshot().unwrap();
    assert_eq!(snapshot.requests.iter().filter(|r| r.method == "tools/list").count(), 1);
    assert_eq!(
        snapshot.calls,
        vec![echo_spoof_mcp::CallRecord {
            name: "test.echo".into(),
            args: json!({"text":"m5-linked"}),
            count: 1,
        }]
    );
    assert!(
        snapshot
            .requests
            .iter()
            .all(|r| r.authorization_matches == Some(true) && !r.dpop_header)
    );
    assert_eq!(h.committed.lock().unwrap().as_slice(), [true, true, true, true]);
    let requests = h.model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].body["input"].as_array().unwrap().last().unwrap()["type"],
        "function_call_output"
    );
    let result: Value = serde_json::from_str(
        requests[1].body["input"].as_array().unwrap().last().unwrap()["output"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        result,
        json!({"content":[{"text":"m5-linked","type":"text"}],"is_error":false})
    );
    let events = trace(&h.path());
    assert_eq!(events.iter().filter(|e| e["event"] == "mcp.grant.reloaded").count(), 1);
    assert_eq!(
        events.iter().filter(|e| e["event"] == "mcp.refresh.committed").count(),
        2
    );
}
fn tool(id: &str) -> Value {
    json!({"type":"function_call","call_id":id,"name":"tool_0","arguments":"{\"text\":\"m5-linked\"}"})
}
#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_strict_healthy_unlink_revokes_once_and_refuses_resource_use() {
    drive(async {
        let h = Harness::new().await;
        let mut session = h.start(true).await;
        h.link(&mut session).await;
        for _ in 0..2 {
            completed(
                &operator(h.directory.path(), "unlink").await.unwrap(),
                0,
                "MCP unlinked\n",
                "",
            );
            assert_relink(&mut session.screen).await;
            assert_eq!(record(h.directory.path())["grant"], Value::Null);
        }
        completed(
            &operator(h.directory.path(), "--refresh-now").await.unwrap(),
            1,
            "",
            "echo-bridge: Grant requires relinking\n",
        );
        assert_relink(&mut session.screen).await;
        journey::wake(&mut session.satellite).await;
        let (_next_wake, wakes) = tokio::sync::mpsc::channel(1);
        let satellite = tokio::spawn(journey::answer(session.satellite, wakes));
        h.live.events.send(event("unlinked", "client")).await.unwrap();
        journey::reach(&mut session.process, &h.path(), "delegation.refused")
            .await
            .unwrap();
        session.process.finish(0).await;
        satellite.await.unwrap();
        assert_eq!(
            h.fixture.counts().unwrap(),
            Counts {
                links: 1,
                polls: 1,
                revokes: 1,
                ..Counts::default()
            }
        );
        assert_eq!(h.spoof.snapshot().unwrap(), echo_spoof_mcp::Snapshot::default());
        assert_eq!(h.model.requests.lock().unwrap().len(), 0);
        assert_eq!(h.committed.lock().unwrap().len(), 0);
        let events = trace(&h.path());
        let refused: Vec<_> = events.iter().filter(|e| e["event"] == "delegation.refused").collect();
        assert_eq!(refused.len(), 1);
        assert_eq!(refused[0]["reason"], "mcp_relink_required");
        assert_eq!(
            events
                .iter()
                .filter(|e| e["event"] == "session.commentary.append")
                .count(),
            0
        );
        h.close().await;
    })
    .await;
}
async fn assert_relink(screen: &mut Lines<BufReader<TcpStream>>) -> echo_screen_contract::UiState {
    let expected =
        json!({"state":"error","message":"Run echo-bridge link with your config to relink Port OS MCP.","relink":true});
    loop {
        let state = bridge_wire_screen::state(screen).await;
        if state["state"] == "error" {
            assert_eq!(state, expected);
            return serde_json::from_value(state).unwrap();
        }
        assert_eq!(state, json!({"state":"listening"}));
    }
}
fn observe_trace(observations: &mut Observations, path: &Path, offset: usize) -> usize {
    let records = trace(path);
    for record in &records[offset..] {
        observations.trace(record).unwrap();
    }
    records.len()
}
fn assert_measurements(h: &Harness, observations: &mut Observations, offset: usize) {
    observe_trace(observations, &h.path(), offset);
    for request in h.model.requests.lock().unwrap().iter() {
        observations.model("m5-linked", &request.body).unwrap();
    }
    let actual = observations
        .measure(h.fixture.counts().unwrap(), &h.spoof.snapshot().unwrap())
        .unwrap();
    assert_eq!(
        actual,
        LinkCounts {
            link: 1,
            tools_list: 1,
            tool_call: 1,
            forced_refresh: 2,
            replay_rejected: 1,
            relink_error: 1,
            restart_grant_kept: 1,
            unlink: 1,
        }
    );
}
async fn replay(h: &Harness, initial: &Value) {
    let url = format!("{}/custom/token", h.fixture.base());
    let jwk = &initial["private_key_jwk"];
    let key = SigningKey::from_slice(&B64.decode(jwk["d"].as_str().unwrap()).unwrap()).unwrap();
    let header = json!({"typ":"dpop+jwt","alg":"ES256","jwk":{"kty":"EC","crv":"P-256","x":jwk["x"],"y":jwk["y"]}});
    let claims = json!({"htm":"POST","htu":url,"iat":h.epoch + 5,"jti":"strict-replay"});
    let input = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
    let signature: Signature = key.sign(input.as_bytes());
    let proof = format!("{input}.{}", B64.encode(signature.to_bytes()));
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(url)
        .header("DPoP", proof)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", "echo-lab"),
            ("refresh_token", initial["grant"]["refresh_token"].as_str().unwrap()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"error":"invalid_grant"})
    );
}
fn assert_replayed(h: &Harness) {
    let snapshot = h.spoof.snapshot().unwrap();
    assert_eq!(snapshot.calls.len(), 1);
    assert_eq!(
        snapshot
            .requests
            .iter()
            .filter(|r| r.authorization_matches == Some(false))
            .count(),
        1
    );
    assert!(snapshot.requests.iter().all(|r| !r.dpop_header));
    assert_eq!(
        h.fixture.counts().unwrap(),
        Counts {
            links: 1,
            polls: 1,
            refreshes: 2,
            replay_rejected: 1,
            revokes: 1
        }
    );
    let events = trace(&h.path());
    for event in [
        "mcp.link.approved",
        "mcp.grant.reloaded",
        "mcp.relink_required",
        "mcp.unlink.completed",
    ] {
        assert_eq!(events.iter().filter(|e| e["event"] == event).count(), 1, "{event}");
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "session.commentary.append")
            .count(),
        1
    );
    let output = std::fs::read_to_string(h.path()).unwrap();
    for private in [
        "synthetic-refresh-",
        "synthetic-device-",
        "synthetic-screen-pair",
        "private_key_jwk",
    ] {
        assert!(!output.contains(private));
    }
}
// Wall time bounds observations even while the Tokio clock is frozen.
pub(super) async fn observe_restart<F: Future>(
    future: F,
    deadline: Instant,
    phase: &str,
    cursor: usize,
) -> Result<F::Output, String> {
    tokio::pin!(future);
    std::future::poll_fn(|cx| {
        if Instant::now() >= deadline {
            return Poll::Ready(Err(format!("restart {phase}: deadline at cursor {cursor}")));
        }
        match future.as_mut().poll(cx) {
            Poll::Ready(value) => Poll::Ready(Ok(value)),
            Poll::Pending => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    })
    .await
}
pub(super) async fn wait_probe(path: &Path, cursor: usize) {
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    let mut checked = false;
    // First Pending witnesses the actual missing record with clocks frozen.
    std::future::poll_fn(|cx| {
        if !checked && !fresh_probe(&std::fs::read(path).unwrap(), cursor) {
            checked = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    observe_restart(
        async { while !fresh_probe(&tokio::fs::read(path).await.unwrap(), cursor) {} },
        deadline,
        "fresh complete idle probe",
        cursor,
    )
    .await
    .unwrap();
}
pub(super) fn assert_no_restart_disconnect(path: &Path) {
    assert!(
        !trace(path)
            .iter()
            .any(|r| r["event"] == "wyoming.reconnecting" && r["elapsed_ms"] == 5000 && r["reason"] == "disconnected")
    );
}
pub(super) async fn assert_restart_deadline<F: Future>(operation: F, deadline: Instant, phase: &str, cursor: usize) {
    tokio::pin!(operation);
    std::future::poll_fn(|cx| {
        assert!(operation.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let failure = observe_restart(operation, deadline, phase, cursor).await.err().unwrap();
    assert_eq!(failure, format!("restart {phase}: deadline at cursor {cursor}"));
}
