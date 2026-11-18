//! T3 owner-local barriers cross the real bridge, pin, media and Wyoming socket.
use super::*;

pub(super) fn report(trace: &Trace, origin: &str, observed: &Value) {
    let completed = trace
        .records
        .iter()
        .filter(|r| r["event"] == "bridge.run_completed")
        .count();
    let played = trace.records.iter().filter(|r| r["event"] == "reply.played").count();
    super::super::provider::driver::report(format_args!(
        "M2 owner {origin}/rtc_local: {observed}; completed={completed} played={played}; capture-only PCM=0; bridge/provider/satellite/listener/home collected=1"
    ));
}

impl Owner {
    async fn hold_second(&mut self) {
        self.wake.send(()).await.unwrap();
        assert_eq!(self.screen.next().await.unwrap()["state"]["state"], "listening");
        self.provider.maintained().await;
        self.receipts(1);
        assert!(!self.bridge.exited().unwrap());
    }

    fn receipts(&self, runs: usize) {
        let rows = self.trace();
        for event in ["bridge.run_completed", "reply.played"] {
            let receipts: Vec<_> = rows.records.iter().filter(|r| r["event"] == event).collect();
            assert_eq!(receipts.len(), runs, "{event}: {:?}", rows.records);
            for (index, receipt) in receipts.iter().enumerate() {
                assert_eq!(receipt["run_id"], format!("run-{}", index + 1));
                if event == "reply.played" {
                    assert_eq!(receipt["frames"], 0);
                    assert_eq!(receipt["dropped_frames"], 0);
                }
            }
        }
        for event in ["live.closed", "tools/call"] {
            assert_eq!(rows.records.iter().filter(|r| r["event"] == event).count(), 0);
        }
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn codex_lifecycle_active_survivor_services_own_peer_through_other_collection() {
    tokio::time::timeout(
        DEADLINE,
        Box::pin(async {
            let (mut first, mut second) = tokio::join!(Owner::start(120, false), Owner::start(120, false));
            assert_ne!(first.address, second.address);
            assert_ne!(first.provider.origin, second.provider.origin);
            assert_ne!(first.root.0, second.root.0);
            tokio::join!(first.run(1), second.run(1));
            second.hold_second().await;
            first.close(1).await;
            second.provider.maintained().await;
            second.receipts(1);
            assert!(!second.bridge.exited().unwrap());
            second.provider.next_capture().await;
            let completed = second.bridge.expect("bridge.run_completed").await.unwrap();
            assert_eq!(completed["run_id"], "run-2");
            assert_eq!(second.screen.next().await.unwrap()["state"]["state"], "ready");
            second.receipts(2);
            second.close(2).await;
            super::super::provider::driver::report(format_args!(
                "F-2: B2 active -> A collected -> B maintenance -> B2 released/completed; retained calls=1 each"
            ));
        }),
    )
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn codex_lifecycle_premature_socket_eof_retains_first_run_and_nonzero_collection() {
    tokio::time::timeout(DEADLINE, Box::pin(async {
        let (mut first, mut second) = tokio::join!(Owner::start(120, false), Owner::start(120, false));
        tokio::join!(first.run(1), second.run(1));
        second.hold_second().await;
        second.provider.quiesce().await.unwrap();
        second.satellite.abort();
        assert!(second.satellite.await.unwrap_err().is_cancelled());
        assert!(second.listener_lifetime.upgrade().is_none());
        let failure = second.bridge.expect("bridge.run_completed").await;
        assert_eq!(failure, Err("process: exited before observation".into()));
        let path = second.output.path().join("bridge.private.jsonl");
        failed_process(second.bridge, second.collected).await;
        let rows = read_trace(&path).unwrap();
        for event in ["bridge.run_completed", "reply.played"] {
            let receipts: Vec<_> = rows.records.iter().filter(|r| r["event"] == event).collect();
            assert_eq!(receipts.len(), 1);
            assert_eq!(receipts[0]["run_id"], "run-1");
        }
        let observed = second.provider.close().await.unwrap();
        assert_eq!(observed, json!({"event":"provider.snapshot", "calls":1, "matching_calls":1,
            "sidebands":1, "external":0, "refreshes":0}));
        remove_root(second.root);
        first.receipts(1);
        first.close(1).await;
        super::super::provider::driver::report(format_args!("F-4: real Wyoming EOF; error=process: exited before observation; child exit=1; B completed/played=1; B2 success=0; each owner collected once"));
    })).await.unwrap();
}
