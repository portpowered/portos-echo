//! Complete-case oracles: missing measurements never mean zero.
use serde_json::{Value, json};

pub fn expected(case: &str) -> Value {
    let mut sequence = vec![
        json!({"state":"ready","message":"Echo Live ready"}),
        json!({"state":"listening"}),
    ];
    if case == "success" {
        sequence.extend([
            json!({"state":"connected"}),
            json!({"state":"speaking"}),
            json!({"state":"ready","message":"Echo Live ready"}),
            json!({"state":"ready","message":"Echo Live ready"}),
        ]);
    } else {
        sequence.clear();
        sequence.push(json!({"state":"error","message":"Run echo-bridge signin with your config, then restart the bridge.","relink":true}));
    }
    json!({"sequence":sequence})
}

pub fn screen(measured: &Value, expected: &Value) -> Result<(), String> {
    let frames = measured["frames"].as_array().ok_or("m2: missing frames")?;
    let states = expected["sequence"].as_array().ok_or("m2: missing expected sequence")?;
    if frames.len() != states.len() {
        return Err(format!(
            "m2: expected {} frames, observed {}",
            states.len(),
            frames.len()
        ));
    }
    for (index, (frame, state)) in frames.iter().zip(states).enumerate() {
        serde_json::from_value::<echo_screen_contract::ScreenFrame>(frame.clone())
            .map_err(|_| "m2: malformed screen frame")?;
        if frame["seq"].as_u64() != u64::try_from(index + 1).ok() || frame["state"] != *state {
            return Err(format!("m2: frame {index}: expected {state}, observed {frame}"));
        }
    }
    Ok(())
}

pub fn trace(measured: &Value, values: &[Value], absent: bool) -> Result<(), String> {
    let records = measured["trace"].as_array().ok_or("m2: missing trace")?;
    if absent {
        for value in values {
            let name = value.as_str().ok_or("m2: invalid absent selector")?;
            if records.iter().any(|r| r["event"] == name) {
                return Err(format!("m2: unexpected {name}"));
            }
        }
        return Ok(());
    }
    for value in values {
        super::super::selectors::check(
            records,
            &super::super::selectors::parse(std::slice::from_ref(value))?,
            None,
        )?;
    }
    Ok(())
}

fn count(measured: &Value, name: &str, expected: u64) -> Result<(), String> {
    let actual = measured[name].as_u64().ok_or_else(|| format!("m2: missing {name}"))?;
    if actual != expected {
        return Err(format!("m2: {name} expected {expected}, observed {actual}"));
    }
    Ok(())
}

pub fn check(measured: &Value) -> Result<(), String> {
    let case = measured["case"].as_str().ok_or("m2: missing case")?;
    if !matches!(case, "success" | "invalid" | "expired") {
        return Err("m2: unknown case".into());
    }
    let success = case == "success";
    count(measured, "calls", u64::from(success))?;
    count(
        measured,
        "refreshes",
        if case == "expired" { EXPIRED_REFRESHES } else { 0 },
    )?;
    if measured["provider_wire"] != true {
        return Err("m2: wrong provider create wire".into());
    }
    let records = measured["trace"].as_array().ok_or("m2: missing trace")?;
    let closes = records.iter().filter(|r| r["event"] == "live.closed").count();
    if closes != usize::from(success) {
        return Err(format!(
            "m2: success close count expected {}, observed {closes}",
            usize::from(success)
        ));
    }
    if success {
        trace(measured, &[json!("live.closed {reason: idle, count: 1}")], false)?;
        trace(measured, &[json!("auth.relink_required"), json!("run.refused")], true)?;
        if measured["runtime"] == "codex" {
            codex_pcm(records)?;
            super::super::m4::wire::audible_pcm(&measured["reply_pcm"])?;
        }
    } else {
        rejection(measured, records)?;
    }
    screen(measured, &expected(case))
}

fn codex_pcm(records: &[Value]) -> Result<(), String> {
    let microphone: Vec<_> = records.iter().filter(|r| r["event"] == "mic.completed").collect();
    let reply: Vec<_> = records.iter().filter(|r| r["event"] == "reply.played").collect();
    let quality: Vec<_> = records.iter().filter(|r| r["event"] == "reply.quality").collect();
    let ([microphone], [reply], [quality]) = (microphone.as_slice(), reply.as_slice(), quality.as_slice()) else {
        return Err("m2: expected one measured microphone and played reply".into());
    };
    if microphone["count"].as_u64().is_none_or(|count| count < 1)
        || microphone["dropped_chunks"] != 0
        || microphone["tail_samples"] != 0
        || reply["outcome"] != "played"
        || reply["frames"] != 10
        || reply["frames"] != reply["received_frames"]
        || reply["dropped_frames"] != 0
        || quality["rtp_lost"] != 0
        || quality["discarded"]["audible"] != 0
        || quality["underruns"].as_u64().is_none()
        || quality["depth_min_ms"].as_u64().is_none()
    {
        return Err("m2: PCM/played accounting failed".into());
    }
    Ok(())
}

// rust-v0.160.0 performs two startup refreshes; remeasure on any pin bump.
pub(super) const EXPIRED_REFRESHES: u64 = 2;

fn rejection(measured: &Value, records: &[Value]) -> Result<(), String> {
    for name in ["wyoming_admissions", "sidebands", "external"] {
        count(measured, name, 0)?;
    }
    count(measured, "exit_code", 1)?;
    for name in ["auth_unchanged", "home_released"] {
        if measured[name] != true {
            return Err(format!("m2: {name} not proved"));
        }
    }
    if measured["stdout"] != "" || measured["stderr"] != "echo-bridge: authorization\n" {
        return Err("m2: unexpected authorization exit output".into());
    }
    if records.iter().filter(|r| r["event"] == "auth.relink_required").count() != 1 {
        return Err("m2: expected exactly one relink event".into());
    }
    trace(
        measured,
        &[
            json!("bridge.ready"),
            json!("run.started"),
            json!("live.connected"),
            json!("mic.completed"),
            json!("reply.played"),
            json!("run.refused"),
        ],
        true,
    )
}
