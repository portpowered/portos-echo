//! Independent T3 oracle: completed bridge PCM writes and projected SSE states.
use super::*;
use tokio::{net::TcpListener, sync::oneshot};

pub(super) async fn exercise(quiet_edges: bool, force_interleaving: bool) {
    let options = super::super::tests::options();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (wake, requested) = oneshot::channel();
    let satellite = tokio::spawn(super::tests::satellite(listener, requested));
    let mut state = Case::default();
    if quiet_edges {
        state.provider = Some(provider::Provider::start_quiet_edges(&options.python).await.unwrap());
    }
    state.start_bridge(&options, port).await.unwrap();
    wake.send(()).unwrap();
    let (pcm, states) = settle(&mut state, force_interleaving).await;
    assert_quiet_edges(&pcm, quiet_edges);
    let admission_epoch = pcm.first().unwrap()["epoch_ms"].clone();
    assert!(
        pcm.iter().all(|row| row["epoch_ms"] == admission_epoch),
        "external scheduling must not advance finite reply admission: {pcm:?}"
    );
    let reply = state.relay.as_ref().unwrap().timing();
    state.provider.as_mut().unwrap().quiesce().await.unwrap();
    advance(state.bridge.as_mut().unwrap(), 120_000).await.unwrap();
    state.bridge.as_mut().unwrap().expect("bridge.ready").await.unwrap();
    let observed = state.close().await.unwrap();
    assert!(provider::valid(&observed), "{observed}");
    assert_eq!(satellite.await.unwrap().0, 10);
    corroborate(&state, &pcm, &states, &reply);
}

fn assert_quiet_edges(pcm: &[Value], required: bool) {
    if required {
        assert_eq!(pcm.first().unwrap()["audible_samples"], 0);
        assert_eq!(pcm.last().unwrap()["audible_samples"], 0);
    }
}

fn corroborate(state: &Case, pcm: &[Value], states: &[Value], reply: &crate::lab_run::sim::m4::wire::Timing) {
    let measured = check(pcm, states);
    assert!(measured.is_ok(), "{measured:?}; pcm={pcm:?}; states={states:?}");
    let measured = measured.unwrap();
    assert_eq!(measured["pcm_bytes"], reply.played);
    assert_eq!(measured["nonzero_samples"], reply.nonzero_samples);
    assert_eq!(measured["audible_samples"], reply.audible_samples);
    let frames = state.frames.lock().unwrap();
    assert_eq!(
        frames.len(),
        states.len() + 1,
        "idle Ready remains a separate publication"
    );
    for (index, (actual, stamp)) in frames.iter().zip(states).enumerate() {
        assert_eq!(actual["seq"], index + 1);
        assert_eq!(actual["state"], stamp["state"]);
    }
    writeln!(
        std::io::stdout(),
        "T3-MOUTH {measured}\nT3-MOUTH states={}",
        json!(states)
    )
    .unwrap();
}

async fn settle(state: &mut Case, force_interleaving: bool) -> (Vec<Value>, Vec<Value>) {
    let mut clock = json!({"epoch_ms":0,"pcm":[],"states":[]});
    let (mut done, mut transcript, mut tail) = (false, false, false);
    let mut released = false;
    while !done {
        state.provider.as_mut().unwrap().tick().await.unwrap();
        if !released {
            released = release_between_snapshot_and_clock(state, &clock, force_interleaving).await;
        }
        let played = state.relay.as_ref().unwrap().timing().played;
        if played >= 2880 && !tail {
            state.provider.as_ref().unwrap().tail().await.unwrap();
            tail = true;
        }
        if played >= 8640 && !transcript {
            state.provider.as_ref().unwrap().transcripts().await.unwrap();
            transcript = true;
        }
        let milliseconds = if released && played < 9600 { 0 } else { 20 };
        done = collect_playout(state.bridge.as_mut().unwrap(), &mut clock, milliseconds)
            .await
            .unwrap()
            .is_some();
    }
    (
        clock["pcm"].as_array().unwrap().clone(),
        clock["states"].as_array().unwrap().clone(),
    )
}

// Force F1's interleaving: the relay snapshot is zero, then real PCM arrives
// before the next clock command. The provider gate must freeze admission time.
async fn release_between_snapshot_and_clock(state: &mut Case, clock: &Value, force_interleaving: bool) -> bool {
    if !connected(clock) {
        return false;
    }
    let relay = state.relay.as_ref().unwrap();
    assert_eq!(relay.timing().played, 0, "closed gate must emit no reply PCM");
    state.provider.as_ref().unwrap().reply().await.unwrap();
    if force_interleaving {
        relay.wait_for_pcm(2880).await.unwrap();
        assert!(relay.timing().played >= 2880, "PCM precedes clock control");
    }
    true
}

use std::io::Write;

fn unsigned(row: &Value, key: &str) -> Result<u64, String> {
    row[key]
        .as_u64()
        .ok_or_else(|| format!("mouth: missing unsigned {key}"))
}

fn audio(pcm: &[Value]) -> Result<Value, String> {
    let (mut cursor, mut first, mut last, mut previous) = (0, None, None, 0);
    let (mut bytes, mut nonzero, mut audible) = (0, 0, 0);
    for row in pcm {
        let written = unsigned(row, "epoch_ms")?;
        let size = unsigned(row, "pcm_bytes")?;
        let nonzero_count = unsigned(row, "nonzero_samples")?;
        let audible_count = unsigned(row, "audible_samples")?;
        if size != 960 || written < previous || audible_count > nonzero_count || nonzero_count > 480 {
            return Err(format!("mouth: malformed PCM frame {row}"));
        }
        previous = written;
        let start = written.max(cursor);
        cursor = start.checked_add(20).ok_or("mouth: PCM clock overflow")?;
        if audible_count > 0 {
            first.get_or_insert(start);
            last = Some(cursor);
        }
        bytes += size;
        nonzero += nonzero_count;
        audible += audible_count;
    }
    if bytes != 9600 || nonzero < 2400 || audible < 2400 {
        return Err(format!(
            "mouth: expected 9600 bytes and >=2400 nonzero/audible samples, got {bytes}/{nonzero}/{audible}"
        ));
    }
    Ok(
        json!({"A":first.ok_or("mouth: no audible start")?, "Z":last.ok_or("mouth: no audible end")?,
        "pcm_bytes":bytes,"nonzero_samples":nonzero,"audible_samples":audible}),
    )
}

fn check(pcm: &[Value], states: &[Value]) -> Result<Value, String> {
    let mut report = audio(pcm)?;
    let expected = ["ready", "listening", "connected", "speaking", "connected", "ready"];
    let actual: Vec<_> = states.iter().map(|row| row["state"]["state"].as_str()).collect();
    if actual != expected.map(Some) {
        return Err(format!("mouth: expected {expected:?}, observed {actual:?}"));
    }
    let mut previous = 0;
    for row in states {
        let timestamp = unsigned(row, "epoch_ms")?;
        if timestamp < previous {
            return Err("mouth: state clock regressed".into());
        }
        previous = timestamp;
    }
    for (name, index, boundary) in [("start", 3, "A"), ("end", 4, "Z")] {
        let timestamp = unsigned(&states[index], "epoch_ms")?;
        let target = unsigned(&report, boundary)?
            .checked_add(200)
            .ok_or("mouth: boundary overflow")?;
        let latest = target.checked_add(20).ok_or("mouth: boundary overflow")?;
        if timestamp < target || timestamp > latest {
            return Err(format!("mouth: {name} at {timestamp}, expected [{target},{latest}]"));
        }
        report[format!("{name}_state_ms")] = json!(timestamp);
        report[format!("{name}_offset_ms")] = json!(i128::from(timestamp) - i128::from(unsigned(&report, boundary)?));
    }
    report["D"] = json!(200);
    report["F"] = json!(20);
    Ok(report)
}

fn fixture() -> (Vec<Value>, Vec<Value>) {
    let pcm = (0..10)
        .map(|_| json!({"epoch_ms":0,"pcm_bytes":960,"nonzero_samples":480,"audible_samples":480}))
        .collect();
    let states = [
        ("ready", 0),
        ("listening", 0),
        ("connected", 0),
        ("speaking", 200),
        ("connected", 400),
        ("ready", 440),
    ]
    .map(|(state, epoch)| json!({"epoch_ms":epoch,"state":{"state":state}}))
    .to_vec();
    (pcm, states)
}

#[test]
fn mouth_oracle_accepts_only_exact_audible_pcm_and_bounded_state_offsets() {
    let (pcm, states) = fixture();
    let measured = check(&pcm, &states).unwrap();
    assert_eq!(measured["start_offset_ms"], 200);
    assert_eq!(measured["end_offset_ms"], 200);
    let mut at_limit = states.clone();
    at_limit[3]["epoch_ms"] = json!(220);
    at_limit[4]["epoch_ms"] = json!(420);
    assert_eq!(check(&pcm, &at_limit).unwrap()["end_offset_ms"], 220);
    for (index, time) in [(3, 199), (3, 221), (4, 399), (4, 421)] {
        let mut wrong = states.clone();
        wrong[index]["epoch_ms"] = json!(time);
        assert!(check(&pcm, &wrong).unwrap_err().contains("expected"));
    }
    for key in ["epoch_ms", "pcm_bytes", "nonzero_samples", "audible_samples"] {
        let mut wrong = pcm.clone();
        wrong[0].as_object_mut().unwrap().remove(key);
        assert!(check(&wrong, &states).unwrap_err().contains("missing unsigned"));
    }
    assert!(check(&pcm[..9], &states).unwrap_err().contains("expected 9600"));
    assert!(check(&[], &states).unwrap_err().contains("expected 9600"));
    let mut silent = pcm.clone();
    for row in &mut silent {
        row["audible_samples"] = json!(0);
    }
    assert!(check(&silent, &states).unwrap_err().contains("expected 9600"));
    let mut extra = pcm;
    extra.push(extra[0].clone());
    assert!(check(&extra, &states).unwrap_err().contains("expected 9600"));
    assert!(check(&extra[..10], &states[..4]).unwrap_err().contains("expected"));
}

#[tokio::test]
async fn t3_controlled_wake_services_idle_probes_without_admitting_a_run() {
    use super::tests::{read, send};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let mut peer = listener.accept().await.unwrap().0;
    let (wake, requested) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let admitted = super::lifecycle::wait_for_wake(&mut peer, async { requested.await.ok() }).await;
        (peer, admitted)
    });
    for _ in 0..2 {
        send(&mut client, "describe", json!({}), vec![]).await;
        assert_eq!(read(&mut client).await.0["type"], "info");
        assert!(!task.is_finished(), "probe admitted a wake");
    }
    // A queued probe remains intact if wake wins the select. Playback can
    // consume it; otherwise the idle reader already answered it.
    send(&mut client, "describe", json!({}), vec![]).await;
    wake.send(()).unwrap();
    let (mut peer, admitted) = task.await.unwrap();
    assert!(admitted);
    send(&mut client, "audio-start", json!({}), vec![]).await;
    let row = read(&mut peer).await.0;
    if row["type"] == "describe" {
        assert_eq!(read(&mut peer).await.0["type"], "audio-start");
    } else {
        assert_eq!(row["type"], "audio-start");
    }
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_mouth_sync_quiet_edges_and_early_played_never_open_early_or_extend_the_mouth() {
    tokio::time::timeout(DEADLINE, Box::pin(exercise(true, false)))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 requires prebuilt production echo-bridge and offline Codex pin"]
async fn t3_mouth_sync_pcm_arriving_after_zero_snapshot_cannot_advance_admission() {
    tokio::time::timeout(DEADLINE, Box::pin(exercise(false, true)))
        .await
        .unwrap();
}

#[test]
fn running_trace_keeps_an_unterminated_record_pending_until_collected() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bridge.private.jsonl");
    let complete = "{\"event\":\"wyoming.probe_ok\",\"elapsed_ms\":1,\"run_id\":\"run-1\"}\n";
    // The bridge has written a record body but not yet its separate newline.
    let partial = format!("{complete}{{\"event\":\"bridge.ready\",\"elapsed_ms\":2}}");
    std::fs::write(&path, &partial).unwrap();
    let running = running_trace(&path).unwrap();
    assert_eq!(running.records.len(), 1);
    assert_eq!(running.records[0]["event"], "wyoming.probe_ok");
    assert_eq!(
        read_trace(&path).err(),
        Some("trace: incomplete final JSON line".into())
    );
    std::fs::write(&path, partial + "\n").unwrap();
    assert_eq!(read_trace(&path).unwrap().records.len(), 2);
}
