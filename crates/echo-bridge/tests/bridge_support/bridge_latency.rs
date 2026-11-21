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

/// CPU clock ticks (1/100 s on Linux) the process has used, user plus system.
fn cpu_ticks(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    // The command name may hold spaces; the numeric fields follow its closing parenthesis.
    let fields: Vec<&str> = stat.rsplit_once(')').unwrap().1.split_whitespace().collect();
    fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
}

const IDLE_PACKS: usize = 4;
const IDLE_PACK_BYTES: usize = 4 * 1024 * 1024;
const IDLE_SETTLE: Duration = Duration::from_secs(5);
const IDLE_WINDOW: Duration = Duration::from_secs(15);
/// 5% of one core over the window, in clock ticks.
const IDLE_BUDGET_TICKS: u64 = 75;

fn install_idle_packs(home: &Path) {
    for index in 0..IDLE_PACKS {
        let pack = home.join(format!("screens/template@1.0.{index}"));
        std::fs::create_dir_all(&pack).unwrap();
        let manifest = json!({"format":1,"name":"template","version":format!("1.0.{index}"),
            "character":{"rest":"rest.png"}});
        std::fs::write(pack.join("pack.json"), manifest.to_string()).unwrap();
        std::fs::write(
            pack.join("rest.png"),
            vec![u8::try_from(index).unwrap(); IDLE_PACK_BYTES],
        )
        .unwrap();
    }
}

// T3: an idle bridge must stay cheap with screen packs installed. The pack watcher once read and hashed every
// installed pack four times a second, which held one core at 100% on the Echo. Measured on this fixture over the
// 15 s window (debug build, pe-ci): 22 ticks healthy (1.5% of a core, the same as with no packs installed) and
// 1037 ticks (69%) with the watcher as it was before the fix. The budget is 75 ticks (5%).
#[tokio::test]
#[ignore = "T3 requires prebuilt echo-bridge"]
async fn bridge_idle_with_installed_screen_packs_stays_under_cpu_budget() {
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    bridge_wire_screen::setup(directory.path(), listener.local_addr().unwrap().port());
    let home = directory.path().join("home");
    install_idle_packs(&home);
    let config = directory.path().join("bridge.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(config, format!("{text}\n[home]\ndir = {:?}\n", home.to_str().unwrap())).unwrap();
    let mut bridge = Process::spawn(&mut bridge_wire_screen::run_command(
        directory.path(),
        "http://127.0.0.1:1",
    ));
    let mut satellite = listener.accept().await.unwrap().0;
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "run-satellite");
    assert_eq!(bridge_short::read(&mut satellite).await.0["type"], "describe");
    bridge_short::send(&mut satellite, "info", json!({})).await;
    assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
    let pid = bridge.child.id().unwrap();

    tokio::time::sleep(IDLE_SETTLE).await;
    let before = cpu_ticks(pid);
    tokio::time::sleep(IDLE_WINDOW).await;
    let used = cpu_ticks(pid) - before;
    writeln!(
        std::io::stdout(),
        "idle bridge cpu: {used} ticks in {} s (budget {IDLE_BUDGET_TICKS})",
        IDLE_WINDOW.as_secs()
    )
    .unwrap();

    // The budget only means something if the bridge was watching these packs.
    let trace_path = directory.path().join("trace");
    let address = trace(&trace_path)
        .iter()
        .find(|e| e["event"] == "screen.listening")
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_owned();
    for index in 0..IDLE_PACKS {
        let file = format!("/screen-packs/v1/template@1.0.{index}/pack.json?token=synthetic-screen-pair");
        let served = health(&address, "GET", &file).await;
        assert!(
            served.starts_with("HTTP/1.1 200 OK\r\n"),
            "pack {index} is not installed"
        );
    }
    assert!(
        used < IDLE_BUDGET_TICKS,
        "idle bridge used {used} CPU ticks in {} s; budget {IDLE_BUDGET_TICKS}",
        IDLE_WINDOW.as_secs()
    );
    drop(satellite);
    bridge.finish(0).await;
}
