//! T3: real task joins and child collection behind the CLI's `world_closed` check.
use super::*;
use std::io::Write;
use tokio::io::AsyncWriteExt;

async fn first_screen() -> (Screen, tokio::net::TcpStream) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connect = Screen::connect(listener.local_addr().unwrap(), true);
    let serve = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\n\r\ndata: {\"seq\":1,\"state\":{\"state\":\"ready\",\"message\":\"Echo Live ready\"}}\n").await.unwrap();
        socket
    };
    let (screen, socket) = tokio::join!(connect, serve);
    (screen.unwrap(), socket)
}

#[tokio::test]
async fn t3_m3_first_pcm_waits_for_admission_trace_before_next_step() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bridge.private.jsonl");
    std::fs::write(&path, "").unwrap();
    let (screen, mut socket) = first_screen().await;
    let mut world = World {
        root: root.path().into(),
        screen: Some(screen),
        first_pcm: Some(960),
        ..World::default()
    };
    let mut ready = Box::pin(world.first_started());
    std::future::poll_fn(|cx| {
        assert!(ready.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    // Model the independent send-receipt observer being scheduled after PCM.
    let record = json!({"event":"reply.started","run_id":"run-1","elapsed_ms":1});
    std::fs::write(path, format!("{record}\n")).unwrap();
    socket
        .write_all(b"data: {\"seq\":2,\"state\":{\"state\":\"speaking\"}}\n")
        .await
        .unwrap();
    assert_eq!(ready.await, Ok(()));
    assert_eq!(world.first_pcm, Some(960));
    assert_eq!(world.trace.records, [record]);
    let expected = super::super::super::selectors::parse(&[json!("reply.started")]).unwrap();
    assert_eq!(
        super::super::super::selectors::check(&world.trace.records, &expected, Some("run-1")),
        Ok(())
    );
}

#[tokio::test(start_paused = true)]
async fn t3_m3_missing_admission_presentation_keeps_original_deadline() {
    let (screen, _socket) = first_screen().await;
    let mut world = World {
        screen: Some(screen),
        ..World::default()
    };
    let mut ready = Box::pin(world.first_started());
    std::future::poll_fn(|cx| {
        assert!(ready.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    assert_eq!(
        ready.await,
        Err("M3 first reply: missing Speaking before 5000 ms deadline".into())
    );
    assert_eq!(world.trace.records, [] as [Value; 0]);
}


impl World {
    pub(super) fn record_release<T, E>(&mut self, owner: &'static str, result: &Result<T, E>) {
        if result.is_ok() {
            self.released.push(owner);
        }
    }
}

async fn listening() -> World {
    let relay = Relay::bind("127.0.0.1:1".parse().unwrap(), &fixture()).await.unwrap();
    let live = Live::bind(
        if cfg!(windows) { "python" } else { "python3" },
        relay.observed.clone(),
        super::super::CutOrder::DoneFirst,
    )
    .await
    .unwrap();
    World {
        relay: Some(relay),
        live: Some(live),
        ..World::default()
    }
}

#[test]
fn barge_fixture_preserves_both_control_orders_and_exact_payloads() {
    use super::super::CutOrder;
    let done_first = CutOrder::DoneFirst.events();
    let user_first = CutOrder::TranscriptFirst.events();
    assert_eq!(done_first.len(), 2);
    assert_eq!(user_first, [done_first[1].clone(), done_first[0].clone()]);
    assert_eq!(done_first[0]["type"], "turn.done");
    assert_eq!(
        done_first[0]["turn"],
        json!({"role":"assistant", "transcript":"The time"})
    );
    assert_eq!(done_first[1]["type"], "input_transcript.added");
    assert_eq!(done_first[1]["item"], json!({"text":"What day is it?"}));
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host and Codex"]
fn t3_m3_done_before_transcript_plays_replacement_and_closes_on_idle() {
    native_case(super::super::CutOrder::DoneFirst, "done_before_transcript");
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host and Codex"]
fn t3_m3_transcript_before_done_plays_replacement_and_closes_on_idle() {
    native_case(super::super::CutOrder::TranscriptFirst, "transcript_before_done");
}

#[test]
#[ignore = "T3 requires prebuilt echo-bridge and pinned hassmic-host and Codex"]
fn t3_m3_delegated_barge_interrupts_once_and_rejects_late_completion() {
    native_case(super::super::CutOrder::Delegated, "delegated");
}

// Test-only artifact selection lets the identical witness fail on an unfixed
// prebuilt bridge. Neither the test nor the driver compiles that artifact.
fn native_bridge() -> PathBuf {
    if let Some(path) = std::env::var_os("ECHO_LAB_M3_BRIDGE") {
        return std::fs::canonicalize(path).unwrap();
    }
    let executable = std::env::current_exe().unwrap();
    let profile = executable
        .ancestors()
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name == "debug" || name == "release")
        })
        .unwrap();
    profile.join(if cfg!(windows) {
        "echo-bridge.exe"
    } else {
        "echo-bridge"
    })
}

fn native_case(order: super::super::CutOrder, name: &str) {
    use crate::lab_run::{Driver, run_options, run_report::Artifacts};
    let bridge = native_bridge();
    let scenario = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/scenarios/m3-full-duplex.yaml");
    let root = tempfile::tempdir().unwrap();
    let options = run_options::parse(&[
        scenario.to_string_lossy().into_owned(),
        "--driver".into(),
        "sim".into(),
        "--bridge".into(),
        bridge.to_string_lossy().into_owned(),
        "--out".into(),
        root.path().join("artifacts").to_string_lossy().into_owned(),
    ])
    .unwrap();
    let (_, _, steps) = crate::lab_run::load(&options).unwrap();
    let mut artifacts = Artifacts::create(options.out.clone()).unwrap();
    let mut driver = super::super::M3::new(&options).unwrap();
    driver.world.cut_order = Some(order);
    let journey = steps
        .iter()
        .enumerate()
        .try_for_each(|(index, step)| driver.execute(step, index, steps.get(index + 1), &mut artifacts));
    let cleanup = driver.close(&mut artifacts);
    if journey.is_err() || cleanup.is_err() {
        native_failure_trace(&options.out, name);
        let retained = root.keep();
        assert!(
            journey.is_ok() && cleanup.is_ok(),
            "journey={journey:?} cleanup={cleanup:?} artifacts={retained:?}"
        );
    }
    assert_eq!(artifacts.checks.get("m3_duplex"), Some(&true));
    assert_eq!(artifacts.checks.get("m3_mute"), Some(&true));
    let events = std::fs::read_to_string(options.out.join("events.jsonl")).unwrap();
    let rows: Vec<Value> = events.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    native_observations(order, name, &rows, &driver.world.trace.records);
}

fn native_failure_trace(root: &Path, name: &str) {
    // The driver copies this payload-free trace before removing its private
    // home, including failed runs. Expose the actual error signature in CI;
    // a missing trace must never be replaced with a synthetic observation.
    if let Ok(trace) = std::fs::read_to_string(root.join("m3.trace.jsonl")) {
        for row in trace
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        {
            if matches!(
                row["event"].as_str(),
                Some("bridge.error" | "live.closed" | "reply.played" | "run.started")
            ) {
                writeln!(std::io::stdout(), "case={name} {row}").unwrap();
            }
        }
    }
}

fn native_observations(order: super::super::CutOrder, name: &str, rows: &[Value], trace: &[Value]) {
    if order.delegated() {
        let interrupts: Vec<_> = trace.iter().filter(|row| row["event"] == "turn/interrupt").collect();
        let receipts: Vec<_> = trace
            .iter()
            .filter(|row| row["event"] == "turn.interrupt_acknowledged")
            .collect();
        assert_eq!(interrupts.len(), 1);
        assert_eq!(receipts.len(), 1);
        let completed: Vec<_> = trace.iter().filter(|row| row["event"] == "turn.interrupted").collect();
        assert_eq!(completed.len(), 1);
        assert!(interrupts[0]["thread_id"].as_str().is_some_and(|id| !id.is_empty()));
        assert!(interrupts[0]["turn_id"].as_str().is_some_and(|id| !id.is_empty()));
        assert_eq!(receipts[0]["thread_id"], interrupts[0]["thread_id"]);
        assert_eq!(receipts[0]["turn_id"], interrupts[0]["turn_id"]);
        assert_eq!(completed[0]["thread_id"], interrupts[0]["thread_id"]);
        assert_eq!(completed[0]["turn_id"], interrupts[0]["turn_id"]);
    }
    // Public scenario events deliberately omit private turn receipts. Assert and
    // print the actual bridge records retained by the production driver.
    for row in rows.iter().chain(trace) {
        if matches!(
            row["event"].as_str(),
            Some(
                "m3.wire"
                    | "m3.duplex"
                    | "m3.mute"
                    | "turn/interrupt"
                    | "turn.interrupt_acknowledged"
                    | "turn.interrupted"
            )
        ) {
            writeln!(std::io::stdout(), "case={name} {row}").unwrap();
        }
    }
}

#[tokio::test]
async fn t3_m3_model_mismatch_reports_failure_without_starting_pending_work() {
    let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
    command
        .arg("-u")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/m3/provider.py"));
    let mut provider = Process::start(&mut command).unwrap();
    let ready = provider.expect("provider.ready").await.unwrap();
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{}/v1/responses", ready["origin"].as_str().unwrap()))
        .json(&json!({"model":"unexpected-fixture-model", "input":[], "tools":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        provider.expect("provider.failure").await.unwrap()["reason"],
        "wrong_model"
    );
    provider.send(&json!({"snapshot":true})).await.unwrap();
    let snapshot = provider.expect("provider.snapshot").await.unwrap();
    assert_eq!(snapshot["models"], 0);
    assert_eq!(snapshot["released"], 0);
    assert_eq!(provider.close().await, Ok(()));
}

#[tokio::test]
async fn t3_m3_model_boundary_holds_completion_until_explicit_release() {
    let mut command = tokio::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
    command
        .arg("-u")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/m3/provider.py"));
    let mut provider = Process::start(&mut command).unwrap();
    let ready = provider.expect("provider.ready").await.unwrap();
    let url = format!("{}/v1/responses", ready["origin"].as_str().unwrap());
    let request = tokio::spawn(async move {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(url)
            .json(&json!({"model":"gpt-5.4", "input":[], "tools":[]}))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    });
    provider.expect("provider.model_started").await.unwrap();
    assert!(!request.is_finished());
    provider.send(&json!({"snapshot":true})).await.unwrap();
    let held = provider.expect("provider.snapshot").await.unwrap();
    assert_eq!(held["models"], 1);
    assert_eq!(held["released"], 0);
    super::super::CutOrder::Delegated.release(&mut provider).await.unwrap();
    let completion = request.await.unwrap();
    let completed: Vec<Value> = completion
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(completed.len(), 3);
    assert_eq!(completed[2]["type"], "response.completed");
    assert_eq!(
        completed[2]["response"]["output"][0]["content"][0]["text"],
        "stale delegated answer"
    );
    super::super::CutOrder::Delegated.verify(&mut provider).await.unwrap();
    assert_eq!(provider.close().await, Ok(()));
}

#[tokio::test]
async fn t3_m3_cleanup_joins_only_its_own_listeners_and_is_idempotent() {
    let (mut first, mut retained) = tokio::join!(listening(), listening());
    let address = retained.relay.as_ref().unwrap().address;
    assert!(first.released.is_empty() && retained.released.is_empty());
    assert_eq!(first.close().await, Ok(()));
    assert_eq!(first.released, ["relay", "live"]);
    assert!(first.relay.is_none() && first.live.is_none());
    assert_eq!(retained.released, [] as [&str; 0]);
    // The other world's listener is still owned, so completed cleanup cannot
    // supply its release observation. Never connect to the closed world.
    let connection = tokio::net::TcpStream::connect(address).await.unwrap();
    drop(connection);
    assert_eq!(retained.close().await, Ok(()));
    assert_eq!(retained.released, ["relay", "live"]);
    assert_eq!(first.close().await, Ok(()));
    assert_eq!(first.released, ["relay", "live"]);
}

async fn child(code: u8) -> (Process, std::sync::mpsc::Receiver<std::process::ExitStatus>) {
    let mut command = tokio::process::Command::new("python3");
    command.args([
        "-u",
        "-c",
        &format!("import sys; print('{{\"event\":\"ready\"}}'); sys.stdin.read(); sys.exit({code})"),
    ]);
    let mut process = Process::start(&mut command).unwrap();
    process.expect("ready").await.unwrap();
    let collected = process.observe_collection();
    assert!(matches!(
        collected.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    (process, collected)
}

#[tokio::test]
async fn t3_m3_cleanup_collects_children_in_order_and_preserves_failure() {
    for code in [0, 7] {
        let mut world = listening().await;
        let (bridge, bridge_exit) = child(code).await;
        let (host, host_exit) = child(0).await;
        world.bridge = Some(bridge);
        world.host = Some(host);
        let root = tempfile::tempdir().unwrap();
        world.root = root.path().join("private");
        std::fs::create_dir(&world.root).unwrap();
        std::fs::write(world.root.join("bridge.private.jsonl"), "").unwrap();
        assert_eq!(world.released, [] as [&str; 0]);
        let result = world.close().await;
        assert_eq!(bridge_exit.try_recv().unwrap().code(), Some(i32::from(code)));
        assert!(host_exit.try_recv().unwrap().success());
        assert!(world.bridge.is_none() && world.host.is_none());
        assert!(world.relay.is_none() && world.live.is_none());
        assert!(!world.root.exists());
        if code == 0 {
            assert_eq!(result, Ok(()));
            assert_eq!(world.released, ["bridge", "relay", "live", "host"]);
        } else {
            assert_eq!(result, Err("cleanup: owned child exited nonzero".into()));
            assert_eq!(world.released, ["relay", "live", "host"]);
        }
    }
}
