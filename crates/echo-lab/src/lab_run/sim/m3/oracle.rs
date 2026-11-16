//! Pure scalar oracle. Missing, stale or malformed observations never pass.
use super::wire::Ledger;
use serde_json::Value;

pub fn duplex(wire: &Ledger, first_pcm: Option<u64>, second_pcm: Option<u64>) -> Result<String, String> {
    if let Some(error) = &wire.failure {
        return Err(error.clone());
    }
    if wire.runs != 1 || wire.inputs != 2 {
        return Err(format!(
            "M3 overlap: expected runs=1 inputs=2, observed runs={} inputs={}",
            wire.runs, wire.inputs
        ));
    }
    if first_pcm.is_none_or(|bytes| bytes < 960 || !bytes.is_multiple_of(2)) {
        return Err("M3 first reply: missing complete decoded host PCM before interruption".into());
    }
    if wire.old_sink_pcm.and_then(|n| usize::try_from(n).ok()) != Some(wire.bytes[0])
        || first_pcm.is_some_and(|n| Some(n) > wire.old_sink_pcm)
    {
        return Err("M3 first reply: missing or inconsistent closed decoded sink PCM".into());
    }
    let cancel = wire.cancel.ok_or("M3 cut: missing provider cancellation")?;
    let first = wire.played.first().ok_or("M3 cut: missing actual first Played")?;
    let elapsed = first
        .checked_duration_since(cancel)
        .ok_or("M3 cut: Played precedes cancellation")?;
    let cut_ms = elapsed.as_secs_f64() * 1000.0;
    if cut_ms >= 600.0 {
        return Err(format!("M3 cut: expected <600 ms, observed {cut_ms:.3} ms"));
    }
    if wire.after_window != 0 || wire.after_stop != 0 {
        return Err(format!(
            "M3 old reply: expected bytes after cancel window=0/stop=0, observed {}/{}",
            wire.after_window, wire.after_stop
        ));
    }
    if wire.played.len() != 2
        || wire.started != 2
        || wire.stops != 2
        || second_pcm != Some(9600)
        || wire.bytes[1] != 9600
    {
        return Err(format!(
            "M3 second answer: expected two Played and 9600 decoded PCM bytes; played={} starts={} stops={} host={second_pcm:?} wire={}",
            wire.played.len(),
            wire.started,
            wire.stops,
            wire.bytes[1]
        ));
    }
    Ok(format!(
        "first_reply_played=true barge_in_cut_ms={cut_ms:.3} barge_in_budget_ms=600 reply_send_window_ms=200 reply_bytes_after_cancel_window=0 second_answer_played=true"
    ))
}

pub fn muted(records: &[Value], before: u64, after: u64) -> Result<String, String> {
    let windows: Vec<_> = records
        .iter()
        .filter(|record| record["event"] == "mic.mute_window")
        .collect();
    let [window] = windows.as_slice() else {
        return Err("M3 mute: expected exactly one closed native mute window".into());
    };
    if window["window_id"] != "mute-1" {
        return Err("M3 mute: stale or invalid window identity".into());
    }
    for name in ["received_frames", "forwarded_frames"] {
        let count = window[name]
            .as_u64()
            .ok_or_else(|| format!("M3 mute: missing or invalid {name}"))?;
        if count != 0 {
            return Err(format!("M3 mute {name}: expected 0, observed {count}"));
        }
    }
    let delta = after
        .checked_sub(before)
        .ok_or("M3 mute: Live frame counter went backwards")?;
    if delta != 0 {
        return Err(format!("M3 muted outbound frames: expected 0, observed {delta}"));
    }
    Ok("muted_outbound_frames=0 received_frames=0 forwarded_frames=0 live_received_frame_delta=0".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::time::{Duration, Instant};

    fn observed() -> Ledger {
        let cancel = Instant::now();
        Ledger {
            runs: 1,
            inputs: 2,
            started: 2,
            stops: 2,
            bytes: [19200, 9600],
            old_sink_pcm: Some(19200),
            cancel: Some(cancel),
            played: vec![cancel + Duration::from_millis(200), cancel + Duration::from_millis(500)],
            ..Ledger::default()
        }
    }

    #[test]
    fn exact_duplex_and_strict_cut_boundary() {
        let mut state = observed();
        assert!(
            duplex(&state, Some(960), Some(9600))
                .unwrap()
                .contains("barge_in_cut_ms=200.000")
        );
        state.played[0] = state.cancel.unwrap() + Duration::from_micros(599_999);
        assert!(duplex(&state, Some(960), Some(9600)).is_ok());
        state.played[0] = state.cancel.unwrap() + Duration::from_millis(600);
        assert!(
            duplex(&state, Some(960), Some(9600))
                .unwrap_err()
                .contains("expected <600")
        );
    }

    #[test]
    fn missing_wrong_phase_partial_and_late_wire_evidence_reject() {
        for (first, second) in [
            (None, Some(9600)),
            (Some(0), Some(9600)),
            (Some(959), Some(9600)),
            (Some(960), None),
            (Some(960), Some(9601)),
        ] {
            assert!(duplex(&observed(), first, second).is_err());
        }
        for field in ["run", "input", "cancel", "played", "window", "stop", "late"] {
            let mut state = observed();
            match field {
                "run" => state.runs = 2,
                "input" => state.inputs = 1,
                "cancel" => state.cancel = None,
                "played" => state.played.pop().map(|_| ()).unwrap(),
                "window" => state.after_window = 960,
                "stop" => state.after_stop = 960,
                _ => state.played[0] = state.cancel.unwrap() - Duration::from_millis(1),
            }
            assert!(duplex(&state, Some(960), Some(9600)).is_err(), "{field}");
        }
    }

    #[test]
    fn closed_mute_window_requires_explicit_zero_and_live_corroboration() {
        let valid = json!({"event":"mic.mute_window","window_id":"mute-1","received_frames":0,"forwarded_frames":0});
        assert_eq!(
            muted(std::slice::from_ref(&valid), 49, 49).unwrap(),
            "muted_outbound_frames=0 received_frames=0 forwarded_frames=0 live_received_frame_delta=0"
        );
        assert!(muted(&[], 49, 49).is_err());
        assert!(muted(&[valid.clone(), valid.clone()], 49, 49).is_err());
        for field in ["received_frames", "forwarded_frames", "window_id"] {
            for bad in [Value::Null, json!(-1), json!(1), json!("stale")] {
                let mut record = valid.clone();
                record[field] = bad;
                assert!(muted(&[record], 49, 49).is_err());
            }
        }
        assert!(
            muted(std::slice::from_ref(&valid), 49, 50)
                .unwrap_err()
                .contains("expected 0, observed 1")
        );
        assert!(muted(&[valid], 49, 48).is_err());
    }
}
