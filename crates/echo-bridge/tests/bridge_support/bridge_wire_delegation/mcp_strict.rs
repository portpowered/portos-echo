//! T3: the delivered bridge owns the grant; section-9 peers enforce the wire.
//! Controlled Wyoming is a component boundary, not the pinned-host M5 scenario.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use echo_lab::lab_mcp::{Approval, Counts, Fixture};
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
        bridge_short::send(
            &mut session.satellite,
            "info",
            json!({"satellite":{"name":"synthetic"}}),
        )
        .await;
        self.fixture.approve(Approval::Approved).unwrap();
        self.fixture.advance(5).unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        session.advance(5000).await;
        completed(&pending.await.unwrap(), 0, "MCP linked\n", "");
        assert_eq!(
            bridge_wire_screen::state(&mut session.screen).await,
            json!({"state":"ready","message":"Ready"})
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
        let resource = self
            .spoof
            .url()
            .strip_prefix("http://")
            .unwrap()
            .strip_suffix("/mcp")
            .unwrap()
            .to_owned();
        let auth = self.fixture.base().strip_prefix("http://").unwrap().to_owned();
        drop(self.live);
        self.spoof.shutdown().await.unwrap();
        self.fixture.shutdown().await.unwrap();
        assert!(TcpStream::connect(resource).await.is_err());
        assert!(TcpStream::connect(auth).await.is_err());
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
        // Completed CLI is the durable-rotation barrier, before any resource use.
        session.process.child.kill().await.unwrap();
        assert!(!session.process.child.wait().await.unwrap().success());
        drop(session);
        let mut restarted = h.start(false).await;
        assert_eq!(bridge_short::read(&mut restarted.satellite).await.0["type"], "describe");
        bridge_short::send(
            &mut restarted.satellite,
            "info",
            json!({"satellite":{"name":"synthetic"}}),
        )
        .await;
        restarted.advance(5000).await;
        h.rotate(&initial, 3).await;
        h.assert_rotations();
        journey::wake(&mut restarted.satellite).await;
        let (_next_wake, wakes) = tokio::sync::mpsc::channel(1);
        let satellite = tokio::spawn(journey::answer(restarted.satellite, wakes));
        h.live.events.send(event("m5-linked", "client")).await.unwrap();
        journey::reach(&mut restarted.process, &h.path(), "session.commentary.append")
            .await
            .unwrap();
        assert_success(&h);
        assert_eq!(
            serde_json::from_str::<Value>(&h.live.commentary.recv().await.unwrap()).unwrap(),
            json!({"type":"session.commentary.append","delegation_id":"m5-linked","text":"m5-linked"})
        );
        replay(&h, &initial).await;
        // A distinct delegated task on the retained Live call must reach MCP.
        h.live.events.send(event("m5-replayed", "client")).await.unwrap();
        journey::reach(&mut restarted.process, &h.path(), "mcp.relink_required")
            .await
            .unwrap();
        assert_relink(&mut restarted.screen).await;
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
        assert_replayed(&h);
        h.close().await;
    })
    .await;
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

async fn assert_relink(screen: &mut Lines<BufReader<TcpStream>>) {
    let expected =
        json!({"state":"error","message":"Run echo-bridge link with your config to relink Port OS MCP.","relink":true});
    loop {
        let state = bridge_wire_screen::state(screen).await;
        if state["state"] == "error" {
            assert_eq!(state, expected);
            break;
        }
        assert_eq!(state, json!({"state":"listening"}));
    }
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
