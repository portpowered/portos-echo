//! Ordered native SSE oracle; measurements always retain received values.
use echo_screen_contract::{ScreenFrame, UiState};
use serde_json::{Value, json};

pub const UTTERANCE: &str = "Computer, show me a happy face";
pub const REPLY: &str = "Here is a happy face.";

pub fn call() -> Value {
    json!({"type":"function_call","call_id":"call-1","name":"tool_3","arguments":"{\"name\":\"happy\"}"})
}

pub fn state() -> Value {
    json!({"state":"emotion","name":"happy","frame_ms":80,"frames":["(^_^)","(^o^)"]})
}

pub fn sequence() -> Vec<Value> {
    vec![
        json!({"state":"ready","message":"Echo Live ready"}),
        json!({"state":"listening"}),
        json!({"state":"connected"}),
        state(),
        json!({"state":"connected"}),
        json!({"state":"speaking"}),
        json!({"state":"ready","message":"Echo Live ready"}),
    ]
}

pub fn frame(actual: &Value, previous: Option<u64>, expected: &Value) -> Result<u64, String> {
    let parsed: ScreenFrame = serde_json::from_value(actual.clone()).map_err(|_| "m6: malformed screen frame")?;
    if previous.is_some_and(|seq| parsed.seq <= seq) {
        return Err("m6: screen sequence must strictly increase".into());
    }
    if let UiState::Emotion { frames, .. } = &parsed.state {
        let distinct = frames.iter().collect::<std::collections::BTreeSet<_>>().len();
        if distinct < 2 {
            return Err(format!(
                "m6: animation frame count {distinct}; expected at least 2 distinct frames"
            ));
        }
    }
    if actual["state"] != *expected {
        return Err(format!("m6: expected state {expected}; observed {}", actual["state"]));
    }
    Ok(parsed.seq)
}

pub fn ordered(actual: &[Value], expected: &Value) -> Result<(), String> {
    let states = expected["sequence"].as_array().ok_or("m6: missing expected sequence")?;
    if expected.as_object().is_none_or(|o| o.len() != 1) || states.len() != actual.len() {
        return Err("m6: ordered screen state count mismatch".into());
    }
    let mut previous = None;
    for (actual, expected) in actual.iter().zip(states) {
        previous = Some(frame(actual, previous, expected)?);
    }
    Ok(())
}

pub fn measurement(world: &super::world::World) -> Value {
    let animation = world
        .ui_states
        .iter()
        .find(|frame| frame["state"]["state"] == "emotion");
    let frame_ms = animation
        .and_then(|frame| frame["state"]["frame_ms"].as_u64())
        .unwrap_or(0);
    let frame_count = animation
        .and_then(|frame| frame["state"]["frames"].as_array())
        .map_or(0, Vec::len);
    let playback = world.playback.as_ref();
    let returned = world
        .ui_states
        .iter()
        .position(|f| f["state"]["state"] == "emotion")
        .and_then(|index| world.ui_states.get(index + 1));
    json!({"ui_states":world.ui_states,"frame_count":frame_count,"frame_ms":frame_ms,
        "return_state":returned.map_or(json!({}), |f| f["state"].clone()),
        "reply_pcm_bytes":world.reply_pcm_bytes,
        "host_pcm_bytes":playback.and_then(|p| p["pcm_bytes"].as_u64()).unwrap_or(0),
        "host_nonzero_bytes":playback.and_then(|p| p["nonzero_bytes"].as_u64()).unwrap_or(0)})
}

pub fn validate_measurement(value: &Value) -> Result<(), String> {
    for frame in value["ui_states"].as_array().ok_or("m6: missing measured states")? {
        serde_json::from_value::<ScreenFrame>(frame.clone()).map_err(|_| "m6: malformed measured screen frame")?;
    }
    // An empty object means no return was observed before failure/cancellation.
    if value["return_state"] != json!({}) {
        serde_json::from_value::<UiState>(value["return_state"].clone())
            .map_err(|_| "m6: malformed measured return state")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
