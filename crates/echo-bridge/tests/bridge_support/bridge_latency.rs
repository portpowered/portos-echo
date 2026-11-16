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
        assert_eq!(host.command("arm").await, "armed");
        assert_eq!(host.command("wake").await, "wake");
        assert_eq!(host.command("cut-ready").await, "cut-ready");
        live.events
            .send(json!({"type":"response.done","response":{"status":"cancelled"}}).to_string())
            .await
            .unwrap();
        let elapsed: f64 = host.command("cut-result").await.parse().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&bridge.line().await).unwrap(),
            json!({"event":"bridge.run_completed","run_id":format!("run-{trial}")})
        );
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
