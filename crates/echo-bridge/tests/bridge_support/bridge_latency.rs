//! T3 mandated real-time measurement: one pinned host and bridge, ten sink EOFs.
use super::*;
use std::io::Write;

#[tokio::test]
#[ignore = "T3 requires make hassmic-host and prebuilt echo-bridge"]
async fn bridge_cut_latency_ten_trials_at_200ms_lead() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let live = bridge_peer::Live::bind_frames(150).await;
    let mut host = Process::spawn(&mut host());
    let port = host.line().await;
    let mut bridge = Process::spawn(&mut bridge(&port, &live.url, &path));
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    let mut values = Vec::new();
    for trial in 1..=10 {
        let before = live.microphone_frames.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(host.command("arm").await, "armed");
        assert_eq!(host.command("wake").await, "wake");
        let ready = host.command("cut-ready").await;
        assert_eq!(
            ready,
            "cut-ready",
            "trial={trial} bridge_exit={:?} trace={:?}",
            bridge.child.try_wait().unwrap(),
            trace(&path)
        );
        live.events
            .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
            .await
            .unwrap();
        let elapsed: f64 = host.command("cut-result").await.parse().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
            json!({"event":"bridge.run_completed","run_id":format!("run-{trial}")})
        );
        assert_microphone(&path, &live, trial, before).await;
        assert!((0.0..600.0).contains(&elapsed), "trial {trial}: {elapsed}ms");
        writeln!(
            std::io::stdout(),
            "cut_latency trial={trial} lead_ms=200 cut_to_sink_close_ms={elapsed:.3}"
        )
        .unwrap();
        values.push(elapsed);
    }
    values.sort_by(f64::total_cmp);
    let median = f64::midpoint(values[4], values[5]);
    writeln!(
        std::io::stdout(),
        "cut_latency trials=10 median_ms={median:.3} max_ms={:.3} threshold_ms=600",
        values[9]
    )
    .unwrap();
    let entries = trace(&path);
    assert_eq!(entries.iter().filter(|e| e["event"] == "reply.cut").count(), 10);
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["event"] == "reply.played" && e["outcome"] == "cut")
            .count(),
        10
    );
    bridge.finish(0).await;
    host.finish(0).await;
}

pub(super) async fn assert_microphone(path: &Path, live: &bridge_peer::Live, run: usize, before: usize) {
    wait_microphone(path, &live.microphone_frames, run, before).await;
    let received = live.microphone_frames.load(std::sync::atomic::Ordering::SeqCst) - before;
    assert_eq!(received, 32);
    let entries = trace(path);
    let mic: Vec<_> = entries
        .iter()
        .filter(|e| e["event"] == "mic.completed" && e["run_id"] == format!("run-{run}"))
        .collect();
    assert_eq!(mic.len(), 1);
    for (field, expected) in [
        ("count", 32),
        ("input_samples", 10240),
        ("tail_samples", 0),
        ("dropped_chunks", 0),
    ] {
        assert_eq!(mic[0][field], expected, "run-{run} {field}: {:?}", mic[0]);
    }
    writeln!(
        std::io::stdout(),
        "run-{run} mic.completed={} fake.received={received} input_samples={} tail_samples={} dropped_chunks={}",
        mic[0]["count"],
        mic[0]["input_samples"],
        mic[0]["tail_samples"],
        mic[0]["dropped_chunks"]
    )
    .unwrap();
}

async fn wait_microphone(path: &Path, frames: &std::sync::atomic::AtomicUsize, run: usize, before: usize) {
    use std::sync::atomic::Ordering::SeqCst;
    let target = before + 32;
    if tokio::time::timeout(OBSERVATION_TIMEOUT, async {
        while frames.load(SeqCst) < target {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .is_err()
    {
        let observed = frames.load(SeqCst);
        let entries: Vec<Value> = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let completion = entries
            .iter()
            .find(|e| e["event"] == "mic.completed" && e["run_id"] == format!("run-{run}"));
        let counters = completion.map_or_else(
            || "no matching mic.completed".to_owned(),
            |mic| {
                format!(
                    "count={} dropped_chunks={} input_samples={} received_chunks={}",
                    mic["count"], mic["dropped_chunks"], mic["input_samples"], mic["received_chunks"]
                )
            },
        );
        assert!(
            observed >= target,
            "run-{run} microphone timeout: observed={observed} before={before} target={target} received={} {counters}",
            observed.saturating_sub(before)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn microphone_timeout_reports_run_counters() {
    for completion in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        let mut entries = vec![json!({"event":"mic.completed","run_id":"run-2","count":99})];
        if completion {
            entries.push(json!({"event":"mic.completed","run_id":"run-1","count":30,
                "dropped_chunks":5,"input_samples":9728,"received_chunks":66}));
        }
        std::fs::write(
            &path,
            entries.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"),
        )
        .unwrap();
        let error = tokio::spawn(async move {
            wait_microphone(&path, &std::sync::atomic::AtomicUsize::new(37), 1, 7).await;
        })
        .await
        .unwrap_err()
        .into_panic();
        let message = error.downcast_ref::<String>().unwrap();
        for expected in ["run-1", "observed=37", "before=7", "target=39", "received=30"] {
            assert!(message.contains(expected), "{message}");
        }
        if completion {
            for expected in [
                "count=30",
                "dropped_chunks=5",
                "input_samples=9728",
                "received_chunks=66",
            ] {
                assert!(message.contains(expected), "{message}");
            }
        } else {
            assert!(message.contains("no matching mic.completed"), "{message}");
        }
        assert!(!message.contains("99"), "{message}");
    }
}
