//! T3: uncertain external mutations never publish or automatically replay a grant.
use super::*;

// Test-owned peer scheduling: hold the complete Info until shared setup's
// next decision is observable. The baseline reaches advance; the fix waits.
#[derive(Debug, PartialEq)]
pub(super) enum ProbeSchedule {
    Held,
    Waiting,
    Advanced,
}

impl Session {
    pub(super) async fn send_probe_reply(&mut self) {
        if self.probe_schedule != Some(ProbeSchedule::Held) {
            bridge_short::send(&mut self.satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
        }
    }

    pub(super) async fn release_at_probe_wait(&mut self, cursor: usize) {
        if self.probe_schedule != Some(ProbeSchedule::Held) {
            return;
        }
        let path = self.trace_path.clone();
        let pending = wait_probe(&path, cursor);
        tokio::pin!(pending);
        // Poll the real readiness wait once with both clocks frozen. This is
        // a scheduling observation, not a timed assertion that nothing happened.
        std::future::poll_fn(|cx| {
            assert!(pending.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        self.probe_schedule = Some(ProbeSchedule::Waiting);
        self.send_probe_reply().await;
        pending.await;
    }

    pub(super) async fn observe_held_advance(&mut self) {
        if self.probe_schedule == Some(ProbeSchedule::Held) {
            self.probe_schedule = Some(ProbeSchedule::Advanced);
            let records = observed(&self.trace_path, "wyoming.reconnecting").await;
            let loss = records.iter().find(|r| r["event"] == "wyoming.reconnecting").unwrap();
            assert_eq!(loss["elapsed_ms"], 5000);
            assert_eq!(loss["reason"], "disconnected");
        }
    }
}

async fn observed(path: &Path, event: &str) -> Vec<Value> {
    let started = std::time::Instant::now();
    loop {
        let bytes = tokio::fs::read(path).await.unwrap();
        let records: Vec<Value> = bytes
            .split_inclusive(|b| *b == b'\n')
            .filter(|line| line.ends_with(b"\n"))
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        if records.iter().any(|r| r["event"] == event) {
            return records;
        }
        assert!(
            started.elapsed() < OBSERVATION_TIMEOUT,
            "did not observe {event}: {records:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn held_info_shared_link_orders_probe_before_clock_and_revoked_resource() {
    drive(async {
        let directory = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        let live = FakeLive::bind(FakeScript {
            events: vec![event("revoked", "client")],
            response: FakeResponse::Answer,
        })
        .await
        .unwrap();
        let model = Model::start(false, true).await;
        let mut session = Session::start_with(
            directory.path(),
            &mock,
            true,
            Some((live.base_url().trim_end_matches('/'), &model.origin)),
        )
        .await;
        session.probe_schedule = Some(ProbeSchedule::Held);
        journey::link(&mut session, &mock, directory.path()).await;
        mock.state.lock().unwrap().revoked = true;
        journey::wake(&mut session.satellite).await;
        let (next, wakes) = tokio::sync::mpsc::channel(1);
        let satellite = tokio::spawn(journey::answer(session.satellite, wakes));
        let reached = journey::reach(&mut session.process, &session.trace_path, "mcp.relink_required").await;
        let records = trace(&session.trace_path);
        assert!(records.iter().any(|r| r["event"] == "mcp.link.approved"));
        assert!(reached.is_ok(), "{}", reached.unwrap_err());
        assert_eq!(session.probe_schedule, Some(ProbeSchedule::Waiting));
        assert_eq!(
            records.iter().filter(|r| r["event"] == "mcp.relink_required").count(),
            1
        );
        assert_eq!(
            records.iter().filter(|r| r["event"] == "wyoming.reconnecting").count(),
            0
        );
        assert_eq!(
            mock.state
                .lock()
                .unwrap()
                .requests
                .iter()
                .filter(|r| r.path == "/mcp")
                .count(),
            1
        );
        observed(&session.trace_path, "mcp.run.failed").await;
        next.send(()).await.unwrap();
        observed(&session.trace_path, "delegation.refused").await;
        session.process.finish(0).await;
        satellite.await.unwrap();
        live.shutdown().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn idle_probe_failed_and_timed_out_emit_no_success_then_recover() {
    drive(async {
        for disconnect in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mock = Mock::bind().await.unwrap();
            let mut session = Session::start(directory.path(), &mock, true).await;
            assert_eq!(bridge_short::read(&mut session.satellite).await.0["type"], "describe");
            if disconnect {
                session.satellite.shutdown().await.unwrap();
            } else {
                session.advance(5000).await;
            }
            let records = observed(&session.trace_path, "wyoming.reconnecting").await;
            assert_eq!(records.iter().filter(|r| r["event"] == "wyoming.probe_ok").count(), 0);
            session.satellite = clock_until(&mut session.process, session.wyoming_listener.accept())
                .await
                .unwrap()
                .0;
            assert_eq!(
                bridge_short::read(&mut session.satellite).await.0["type"],
                "run-satellite"
            );
            assert_eq!(
                clock_until(&mut session.process, bridge_short::read(&mut session.satellite))
                    .await
                    .0["type"],
                "describe"
            );
            session.answer_probe().await;
            let records = trace(&session.trace_path);
            assert_eq!(records.iter().filter(|r| r["event"] == "wyoming.probe_ok").count(), 1);
            assert_eq!(records.iter().filter(|r| r["event"] == "wyoming.connected").count(), 2);
            session.process.finish(0).await;
        }
    })
    .await;
}

async fn clock_until<F: Future>(process: &mut Process, future: F) -> F::Output {
    tokio::pin!(future);
    for _ in 0..1500 {
        if let Poll::Ready(value) = std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await {
            return value;
        }
        // Never cancel command(): its acknowledgement belongs to this tick.
        let line = process.command(r#"{"action":"advance_clock","milliseconds":20}"#).await;
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            json!({"event":"bridge.clock_advanced","milliseconds":20})
        );
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    unreachable!("recovery did not reach peer boundary within existing simulated ceiling")
}

const STORAGE: &str = "Grant storage failed; rotation may require recovery";
const TRANSPORT: &str = "OAuth transport failed; operation may be incomplete";

#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_failed_rotation_storage_or_transport_blocks_use_and_replay() {
    drive(async {
        for storage in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let mock = Mock::bind().await.unwrap();
            let mut session = Session::start(directory.path(), &mock, true).await;
            journey::link(&mut session, &mock, directory.path()).await;
            let original = record(directory.path());
            let path = directory.path().join("portos/grant.json");
            let retained = directory.path().join("retained.private.json");
            let message = if storage {
                std::fs::rename(&path, &retained).unwrap();
                std::fs::create_dir(&path).unwrap();
                STORAGE
            } else {
                mock.state.lock().unwrap().fault = Some(("/custom/token".into(), 503, json!({"private":"do not print"})));
                TRANSPORT
            };
            completed(&operator(directory.path(), "--refresh-now").await.unwrap(), 1, "", &format!("echo-bridge: {message}\n"));
            assert_eq!(bridge_wire_screen::state(&mut session.screen).await, json!({"state":"error","message":message,"relink":true}));
            if storage {
                assert_eq!(mock.state.lock().unwrap().generation, 2);
                // Repair only the fixture's filesystem obstacle. This must not
                // clear the running owner's uncertain mutation latch.
                std::fs::remove_dir(&path).unwrap();
                std::fs::rename(retained, &path).unwrap();
            }
            assert!(record(directory.path()) == original, "uncommitted grant replaced durable state");
            completed(&operator(directory.path(), "--refresh-now").await.unwrap(), 1, "", "echo-bridge: Grant requires relinking\n");
            assert_eq!(bridge_wire_screen::state(&mut session.screen).await, json!({"state":"error","message":"Run echo-bridge link with your config to relink Port OS MCP.","relink":true}));
            session.process.finish(0).await;
            assert_uncertain(&mock, directory.path());
        }
    }).await;
}

#[tokio::test(start_paused = true)]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn mcp_link_cancelled_rotated_response_never_replays_or_publishes() {
    drive(async {
        let directory = tempfile::tempdir().unwrap();
        let mock = Mock::bind().await.unwrap();
        let mut session = Session::start(directory.path(), &mock, true).await;
        journey::link(&mut session, &mock, directory.path()).await;
        let original = record(directory.path());
        mock.state.lock().unwrap().hold = Some("/custom/token".into());
        let address: std::net::SocketAddr = session.control.parse().unwrap();
        assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
        let mut url = reqwest::Url::parse("http://127.0.0.1/operator/mcp").unwrap();
        url.set_port(Some(address.port())).unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let request = tokio::spawn(client.post(url).bearer_auth("synthetic-screen-pair").json(&json!({"operation":"refresh"})).send());
        while !mock.state.lock().unwrap().held { tokio::task::yield_now().await; }
        assert_eq!(mock.state.lock().unwrap().generation, 2);
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert_eq!(bridge_wire_screen::state(&mut session.screen).await, json!({"state":"error","message":"Run echo-bridge link with your config to relink Port OS MCP.","relink":true}));
        mock.release.notify_one();
        completed(&operator(directory.path(), "--refresh-now").await.unwrap(), 1, "", "echo-bridge: Grant requires relinking\n");
        assert!(record(directory.path()) == original, "cancelled mutation published grant");
        session.process.finish(0).await;
        assert_uncertain(&mock, directory.path());
    }).await;
}

fn assert_uncertain(mock: &Mock, directory: &Path) {
    let state = mock.state.lock().unwrap();
    assert_eq!(state.requests.iter().filter(|r| r.path == "/custom/link").count(), 1);
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|r| r.form.get("grant_type").is_some_and(|v| v == "refresh_token"))
            .count(),
        1
    );
    assert_eq!(state.requests.iter().filter(|r| r.path == "/mcp").count(), 0);
    let events = trace(&directory.join("trace"));
    assert_eq!(
        events.iter().filter(|e| e["event"] == "mcp.refresh.committed").count(),
        0
    );
    let text = std::fs::read_to_string(directory.join("trace")).unwrap();
    for private in ["do not print", "refresh-1", "access-2", "private_key_jwk"] {
        assert!(!text.contains(private));
    }
}

pub(super) async fn advance_with_probes(session: &mut Session) {
    // A five-second jump can expire an idle describe whose info is still in
    // flight. Keep the same total elapsed time and service the real peer while
    // advancing in the journey's existing twenty-millisecond clock steps.
    for _ in 0..250 {
        let advance = session
            .process
            .command(r#"{"action":"advance_clock","milliseconds":20}"#);
        tokio::pin!(advance);
        loop {
            tokio::select! {
                ready = session.satellite.readable() => {
                    ready.unwrap();
                    // Select readiness before reading, so a completed clock
                    // command cannot discard a partially buffered header.
                    let mut first = [0];
                    match session.satellite.try_read(&mut first) {
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                        result => assert_eq!(result.unwrap(), 1),
                    }
                    let mut frame = (&first[..]).chain(&mut session.satellite);
                    let (header, payload) = bridge_short::read(&mut frame).await;
                    assert_eq!(header["type"], "describe");
                    assert_eq!(payload, Vec::<u8>::new());
                    bridge_short::send(&mut session.satellite, "info", json!({"satellite":{"name":"synthetic"}})).await;
                }
                line = &mut advance => {
                    assert_eq!(serde_json::from_str::<Value>(&line).unwrap(),
                        json!({"event":"bridge.clock_advanced","milliseconds":20}));
                    break;
                }
            }
        }
    }
}
