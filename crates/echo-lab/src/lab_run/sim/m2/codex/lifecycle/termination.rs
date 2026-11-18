//! T3 causal schedules use real bridge/pin/media and only gate peer consumption.
use super::*;
use peer::{Observer, Phase};

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn codex_lifecycle_end_then_eof_before_controller_collection() {
    tokio::time::timeout(DEADLINE, Box::pin(kill_schedule(false)))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn codex_lifecycle_buffered_end_with_collection_ready_before_eof() {
    tokio::time::timeout(DEADLINE, Box::pin(kill_schedule(true)))
        .await
        .unwrap();
}

async fn kill_schedule(collect_first: bool) {
    let (events, mut observed) = mpsc::unbounded_channel();
    let (resume, permitted) = mpsc::channel(1);
    let mut owner = Owner::start_observed(
        120,
        false,
        Some(Observer {
            events,
            resume: permitted,
        }),
    )
    .await;
    owner.run(1).await;
    owner.provider.quiesce().await.unwrap();
    owner.wake.send(()).await.unwrap();
    assert_eq!(owner.screen.next().await.unwrap()["state"]["state"], "listening");
    owner.provider.next_capture().await;
    at_phase(&mut observed, Phase::ReplyStopped).await;
    kill(&mut owner).await;
    resume.send(()).await.unwrap();
    at_phase(&mut observed, Phase::CutAcknowledged).await;
    assert_killed(&mut owner).await;
    collect(owner, observed, resume, collect_first).await;
}

async fn collect(
    owner: Owner,
    mut observed: mpsc::UnboundedReceiver<Phase>,
    resume: mpsc::Sender<()>,
    collect_first: bool,
) {
    let mut wake = Some(owner.wake);
    let mut counts = Counts::default();
    before_shutdown(collect_first, &resume, &mut observed, &mut counts).await;
    close_bridge(owner.bridge, owner.collected, &owner.listener_lifetime).await;
    if collect_first {
        drop(wake.take());
        resume.send(()).await.unwrap();
    }
    through(&mut observed, &mut counts, Phase::Eof).await;
    assert_eq!(counts.refusals, 1);
    assert!(counts.ends >= 1, "shutdown end must be consumed in idle phase");
    assert!(owner.listener_lifetime.upgrade().is_some());
    drop(wake.take());
    resume.send(()).await.unwrap();
    assert_eq!(owner.satellite.await.unwrap(), 2);
    assert!(owner.listener_lifetime.upgrade().is_none());
    finish(owner.root, owner.output, owner.provider, collect_first, counts.ends).await;
}

async fn finish(
    root: config::Root,
    output: tempfile::TempDir,
    provider: provider::Provider,
    collect_first: bool,
    idle_ends: usize,
) {
    let rows = read_trace(&output.path().join("bridge.private.jsonl")).unwrap();
    final_receipts(&rows);
    let provider = provider.close().await.unwrap();
    assert_eq!(
        provider,
        json!({"event":"provider.snapshot", "calls":1, "matching_calls":1,
        "sidebands":1, "external":0, "refreshes":0})
    );
    remove_root(root);
    super::super::provider::driver::report(format_args!(
        "kill schedule collect_first={collect_first}: completed=1 played=1 kill=1 late_success=0 reply_PCM=0 refusal=1 idle_end={idle_ends} collection=1 released=1; {provider}"
    ));
}

async fn kill(owner: &mut Owner) {
    let options = super::super::super::tests::options();
    let output = tokio::process::Command::new(options.bridge.unwrap())
        .current_dir(owner.root.0.join("configured.private"))
        .args(["kill", "--config", "bridge.toml"])
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"kill-switch: on\n");
    assert_eq!(output.stderr, b"");
}

async fn assert_killed(owner: &mut Owner) {
    assert_eq!(
        owner.screen.next().await.unwrap()["state"],
        json!({
            "state":"error", "relink":false,
            "message":"Kill switch enabled. Run echo-bridge kill --clear with your config."
        })
    );
}

fn final_receipts(rows: &Trace) {
    for event in ["bridge.run_completed", "reply.played"] {
        let receipts: Vec<_> = rows.records.iter().filter(|r| r["event"] == event).collect();
        assert_eq!(receipts.len(), 1, "{event}: {:?}", rows.records);
        assert_eq!(receipts[0]["run_id"], "run-1");
        if event == "reply.played" {
            assert_eq!(receipts[0]["frames"], 0);
            assert_eq!(receipts[0]["dropped_frames"], 0);
        }
    }
    let closed: Vec<_> = rows.records.iter().filter(|r| r["event"] == "live.closed").collect();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0]["reason"], "kill");
}

#[tokio::test]
async fn t3_codex_lifecycle_idle_end_is_validated_and_listener_waits_for_collection() {
    tokio::time::timeout(DEADLINE, async {
        let (listener, lifetime) = observed_listener().await;
        let address = listener.local_addr().unwrap();
        let (wake, requested) = mpsc::channel(1);
        let (events, mut observed) = mpsc::unbounded_channel();
        let (resume, permitted) = mpsc::channel(1);
        let task = tokio::spawn(peer::observed_satellite(
            listener,
            requested,
            false,
            Some(Observer {
                events,
                resume: permitted,
            }),
        ));
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        super::super::tests::send(&mut client, "run-satellite", json!({}), vec![]).await;
        super::super::tests::send(&mut client, "describe", json!({}), vec![]).await;
        assert_eq!(super::super::tests::read(&mut client).await.0["type"], "info");
        client.writable().await.unwrap();
        tokio::io::AsyncWriteExt::write_all(
            &mut client,
            b"{\"type\":\"hassmic-end\",\"data\":null,\"data_length\":0,\"payload_length\":0}\n",
        )
        .await
        .unwrap();
        assert_eq!(observed.recv().await.unwrap(), Phase::Control("hassmic-end".into()));
        drop(client);
        assert_eq!(observed.recv().await.unwrap(), Phase::Eof);
        assert!(lifetime.upgrade().is_some());
        drop(wake);
        resume.send(()).await.unwrap();
        assert_eq!(task.await.unwrap(), 0);
        assert!(lifetime.upgrade().is_none());
    })
    .await
    .unwrap();
}

#[derive(Default)]
struct Counts {
    ends: usize,
    refusals: usize,
}

impl Counts {
    fn record(&mut self, phase: Phase) {
        match phase {
            Phase::Control(kind) if kind == "hassmic-end" => self.ends += 1,
            Phase::Control(kind) if kind == "error" => self.refusals += 1,
            Phase::Control(kind) => assert_eq!(kind, "describe"),
            other => assert_eq!(other, Phase::PlaybackEnded),
        }
    }
}

async fn through(observed: &mut mpsc::UnboundedReceiver<Phase>, counts: &mut Counts, until: Phase) {
    loop {
        let phase = observed.recv().await.unwrap();
        if phase == until {
            if phase != Phase::Eof {
                counts.record(phase);
            }
            return;
        }
        counts.record(phase);
    }
}

async fn at_phase(observed: &mut mpsc::UnboundedReceiver<Phase>, until: Phase) {
    loop {
        let phase = observed.recv().await.unwrap();
        if phase == until {
            return;
        }
        assert_eq!(phase, Phase::Control("describe".into()));
    }
}

async fn close_bridge(
    mut bridge: Process,
    collected: std::sync::mpsc::Receiver<std::process::ExitStatus>,
    lifetime: &std::sync::Weak<TcpListener>,
) {
    assert_eq!(bridge.close().await, Ok(()));
    drop(bridge);
    assert!(collected.try_recv().unwrap().success());
    assert!(lifetime.upgrade().is_some());
}

async fn before_shutdown(
    collect_first: bool,
    resume: &mpsc::Sender<()>,
    observed: &mut mpsc::UnboundedReceiver<Phase>,
    counts: &mut Counts,
) {
    if !collect_first {
        resume.send(()).await.unwrap();
        through(observed, counts, Phase::Control("error".into())).await;
    }
}
