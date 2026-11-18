//! Independent T3 oracle: completed bridge PCM writes and projected SSE states.
use super::*;
use tokio::{net::TcpListener, sync::oneshot};

pub(super) async fn exercise(quiet_edges: bool) {
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
    let (pcm, states) = settle(&mut state).await;
    assert_quiet_edges(&pcm, quiet_edges);
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

async fn settle(state: &mut Case) -> (Vec<Value>, Vec<Value>) {
    let (mut pcm, mut states) = (vec![], vec![]);
    let (mut done, mut transcript, mut tail) = (false, false, false);
    while !done {
        state.provider.as_mut().unwrap().tick().await.unwrap();
        let played = state.relay.as_ref().unwrap().timing().played;
        if state.relay.as_ref().unwrap().timing().played >= 2880 && !tail {
            state.provider.as_ref().unwrap().tail().await.unwrap();
            tail = true;
        }
        if state.relay.as_ref().unwrap().timing().played >= 8640 && !transcript {
            state.provider.as_ref().unwrap().transcripts().await.unwrap();
            transcript = true;
        }
        let bridge = state.bridge.as_mut().unwrap();
        let milliseconds = if played > 0 && played < 9600 { 0 } else { 20 };
        bridge
            .send(&json!({"action":"advance_playout","milliseconds":milliseconds}))
            .await
            .unwrap();
        loop {
            let row = bridge.line().await.unwrap();
            match row["event"].as_str().unwrap() {
                "bridge.run_completed" => {
                    assert_eq!(row["run_id"], "run-1");
                    done = true;
                }
                "bridge.clock_advanced" => {
                    assert_eq!(row["milliseconds"], milliseconds);
                    assert_eq!(row["epoch_ms"].as_u64().unwrap() % 20, 0);
                    pcm.extend(row["pcm"].as_array().unwrap().iter().cloned());
                    states.extend(row["states"].as_array().unwrap().iter().cloned());
                    break;
                }
                other => assert_eq!(other, "expected clock or completion receipt", "{row}"),
            }
        }
    }
    (pcm, states)
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
    let expected = ["ready", "listening", "connected", "speaking", "ready"];
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
        ("ready", 400),
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
