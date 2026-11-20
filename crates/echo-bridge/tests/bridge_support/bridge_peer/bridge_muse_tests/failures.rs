//! T3: report isolation, fake-clock deadline, kill and duplicate delegation.
use super::*;

struct Rig {
    directory: tempfile::TempDir,
    muse: Muse,
    host: Process,
    bridge: Process,
    live: bridge_peer::Live,
    model: bridge_wire_delegation::Model,
    id: String,
}

impl Rig {
    async fn start() -> Self {
        Self::start_with_clock(false).await
    }

    async fn start_with_clock(clock: bool) -> Self {
        let muse = Muse::start("subjectAltName=IP:127.0.0.1").await;
        let directory = tempfile::tempdir().unwrap();
        let mut host = Process::spawn(&mut host());
        let port = host.line().await.parse().unwrap();
        configure(directory.path(), port, &muse, "");
        let model = bridge_wire_delegation::Model::start(false, true).await;
        let live = bridge_peer::Live::bind_frames(0).await;
        let mut command = bridge_wire_delegation::run_command(directory.path(), &live.url, &model.origin);
        if clock {
            command.arg("--sim-clock");
        }
        let mut bridge = Process::spawn(&mut command);
        assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
        assert_eq!(host.command("wake").await, "wake");
        let path = directory.path().join("trace");
        wait(&path, "mic.frames").await;
        live.events
            .send(bridge_wire_delegation::event("muse-d-1", "client"))
            .await
            .unwrap();
        let progress = wait(&path, "muse.progress").await;
        let id = progress["task_id"].as_str().unwrap().to_owned();
        assert_eq!(progress["delegation_id"], "muse-d-1");
        assert_eq!(muse.calls().len(), 1);
        check_gadget(&muse.calls()[0], &id);
        assert!(live.commentary.is_empty());
        Self {
            directory,
            muse,
            host,
            bridge,
            live,
            model,
            id,
        }
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join("trace")
    }

    async fn reject(&self, id: &str, error: &str) {
        let output = self
            .muse
            .report_output(id, "completed", "Late success must never be spoken")
            .await;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(String::from_utf8(output.stderr).unwrap(), format!("{error}\n"));
    }

    async fn reply(&mut self, text: &str, status: &str) {
        let path = self.path();
        reply(&mut self.live, &mut self.bridge, &mut self.host, &path, text).await;
        let entries = trace(&path);
        let terminals: Vec<_> = entries.iter().filter(|e| e["event"] == "muse.terminal").collect();
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0]["task_id"], self.id);
        assert_eq!(terminals[0]["delegation_id"], "muse-d-1");
        assert_eq!(terminals[0]["run_id"], "run-1");
        assert_eq!(terminals[0]["status"], status);
        let outcome = if text == "Muse did not report a result in time" {
            "timed_out"
        } else {
            status
        };
        check_resolution(
            &path,
            outcome,
            "submitted",
            Some(text),
            (outcome == "failed").then_some("task_failed"),
        );
        if status == "failed" {
            assert_eq!(terminals[0]["retryable"], false);
        }
        assert_eq!(self.model.request_count(), 0);
    }

    async fn close(mut self, calls: usize) {
        self.bridge.finish(0).await;
        self.host.finish(0).await;
        assert_eq!(self.model.request_count(), 0);
        assert!(self.live.commentary.try_recv().is_err());
        assert_eq!(self.muse.calls().len(), calls);
        let entries = trace(&self.path());
        assert_eq!(entries.iter().filter(|e| e["event"] == "tools/call").count(), 0);
        let microphone: Vec<_> = entries.iter().filter(|e| e["event"] == "mic.completed").collect();
        assert_eq!(microphone.len(), 1);
        assert_eq!(microphone[0]["dropped_chunks"], 0);
        evidence(&format!("muse microphone: {}", microphone[0]));
        let raw = std::fs::read_to_string(self.path()).unwrap();
        for private in ["Late success", "synthetic-api-key", "muse-credential", "Done!"] {
            assert!(!raw.contains(private));
        }
        self.muse.stop().await;
    }
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt bridge/Muse binaries; Linux and openssl"]
async fn muse_unknown_and_duplicate_reports_cannot_complete_another_task() {
    let mut rig = Rig::start().await;
    rig.reject("unknown-task", "unknown task_id").await;
    assert!(rig.live.commentary.try_recv().is_err());
    assert_eq!(
        trace(&rig.path())
            .iter()
            .filter(|e| e["event"] == "muse.terminal")
            .count(),
        0
    );
    assert_eq!(
        rig.muse.report(&rig.id).await,
        json!({"accepted":true,"task_id":rig.id})
    );
    rig.reply("Muse test job completed", "completed").await;
    rig.reject(&rig.id, "task_id already reported").await;
    evidence(&format!(
        "muse unknown/duplicate: task_id={} unknown_exit=1 duplicate_exit=1 terminal=1 commentary=1 pcm_bytes=9600",
        rig.id
    ));
    rig.close(1).await;
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt bridge/Muse binaries; Linux and openssl"]
async fn muse_run_ceiling_cancels_and_next_wake_plays() {
    let mut rig = Rig::start_with_clock(true).await;
    let receipt = rig
        .bridge
        .command(r#"{"action":"advance_clock","milliseconds":600000}"#)
        .await;
    assert_eq!(
        serde_json::from_str::<Value>(&receipt).unwrap(),
        json!({"event":"bridge.clock_advanced","milliseconds":600000})
    );
    let failed = wait(&rig.path(), "run.failed").await;
    assert_eq!(failed["reason"], "session waiting ceiling reached");
    assert_eq!(failed["run_id"], "run-1");
    rig.reject(&rig.id, "cancelled task_id").await;
    check_resolution(&rig.path(), "cancelled", "unobserved", None, None);
    let calls = rig.muse.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        &calls[1]["argv"].as_array().unwrap()[..5],
        &calls[0]["argv"].as_array().unwrap()[..5]
    );
    assert_eq!(
        calls[1]["argv"][5],
        format!("Cancel task {}. Do not call echo.report.result for it.", rig.id)
    );
    assert!(rig.live.commentary.try_recv().is_err());
    assert_eq!(rig.host.command("pcm").await, "0 0");
    assert_eq!(rig.host.command("wake").await, "wake");
    let path = rig.path();
    let next = wait_run(&path, "mic.frames", "run-2").await;
    assert_eq!(next["phase"], "first");
    rig.live
        .events
        .send(bridge_wire_delegation::event("muse-d-2", "client"))
        .await
        .unwrap();
    let progress = wait_run(&path, "muse.progress", "run-2").await;
    let id = progress["task_id"].as_str().unwrap();
    assert_ne!(id, rig.id);
    rig.muse.report(id).await;
    play_reply(
        &mut rig.live,
        &mut rig.bridge,
        &mut rig.host,
        &path,
        ("muse-d-2", "Muse test job completed", "run-2"),
        true,
    )
    .await;
    assert_eq!(
        wait_run(&path, "session.commentary.append", "run-2").await["delegation_id"],
        "muse-d-2"
    );
    assert_eq!(
        trace(&path)
            .iter()
            .filter(|e| e["event"] == "session.commentary.append")
            .count(),
        1
    );
    assert_eq!(rig.muse.calls().len(), 3);
    assert_eq!(rig.model.request_count(), 0);
    rig.bridge.finish(0).await;
    rig.host.finish(0).await;
    rig.muse.stop().await;
}

async fn wait_run(path: &Path, event: &str, run: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(row) = trace(path)
                .into_iter()
                .find(|row| row["event"] == event && row["run_id"] == run)
            {
                return row;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("waiting for {event} in {run}: {:?}", trace(path)))
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt bridge/Muse binaries; Linux and openssl"]
async fn muse_failed_callback_speaks_one_honest_nonretryable_result() {
    let mut rig = Rig::start().await;
    let output = rig.muse.report_output(&rig.id, "failed", "Muse test job failed").await;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stderr, b"");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"accepted":true,"task_id":rig.id})
    );
    rig.reply("Muse test job failed", "failed").await;
    rig.reject(&rig.id, "task_id already reported").await;
    evidence(&format!(
        "muse failed report: task_id={} failed=1 retryable=false completed=0 commentary=1 pcm_bytes=9600",
        rig.id
    ));
    rig.close(1).await;
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt bridge/Muse binaries; Linux and openssl"]
async fn muse_duplicate_and_busy_delegations_submit_only_once() {
    let mut rig = Rig::start().await;
    rig.live
        .events
        .send(bridge_wire_delegation::event("muse-d-1", "client"))
        .await
        .unwrap();
    rig.live
        .events
        .send(bridge_wire_delegation::event("busy-d-2", "client"))
        .await
        .unwrap();
    let refused = wait(&rig.path(), "delegation.refused").await;
    assert_eq!(refused["delegation_id"], "busy-d-2");
    assert_eq!(refused["reason"], "target_or_busy");
    assert_eq!(rig.muse.calls().len(), 1);
    rig.muse.report(&rig.id).await;
    rig.reply("Muse test job completed", "completed").await;
    check_trace(&rig.path(), Some(&rig.id));
    evidence(&format!(
        "muse duplicate/busy: task_id={} submits=1 busy_submits=0 terminal=1 commentary=1 pcm_bytes=9600",
        rig.id
    ));
    rig.close(1).await;
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt bridge/Muse binaries; Linux and openssl"]
async fn muse_kill_cancels_once_and_late_report_never_speaks() {
    let mut rig = Rig::start().await;
    bridge_wire_auth::invoke(
        rig.directory.path(),
        &["kill", "--config", "bridge.toml"],
        0,
        "kill-switch: on\n",
    )
    .await;
    wait(&rig.path(), "live.closed").await;
    rig.reject(&rig.id, "cancelled task_id").await;
    let calls = rig.muse.calls();
    assert_eq!(calls.len(), 2);
    let first = calls[0]["argv"].as_array().unwrap();
    let second = calls[1]["argv"].as_array().unwrap();
    assert_eq!(&second[..5], &first[..5]);
    assert_eq!(
        second[5],
        format!("Cancel task {}. Do not call echo.report.result for it.", rig.id)
    );
    let entries = trace(&rig.path());
    check_resolution(&rig.path(), "cancelled", "unobserved", None, None);
    // hassmic acknowledges the empty audio-stop cut as played. Actual decoded
    // PCM and commentary, rather than that acknowledgement, witness no speech.
    for event in ["muse.terminal", "session.commentary.append"] {
        assert_eq!(entries.iter().filter(|e| e["event"] == event).count(), 0, "{event}");
    }
    assert!(rig.live.commentary.try_recv().is_err());
    assert_eq!(rig.host.command("pcm").await, "0 0");
    evidence(&format!(
        "muse kill: task_id={} cancel_advisories=1 same_session=true late_exit=1 terminal=0 commentary=0 pcm_bytes=0",
        rig.id
    ));
    rig.close(2).await;
}
