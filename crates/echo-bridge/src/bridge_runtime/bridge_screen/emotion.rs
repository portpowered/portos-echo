//! Only successful typed results of the spoof emotion tool can project a face.
use echo_screen_contract::UiState;
use serde_json::Value;
use std::time::Duration;
use tokio::time::Instant;

pub(crate) fn decode(name: &str, content: &Value, is_error: bool) -> Option<(UiState, Instant)> {
    if name != "ui.show_emotion" || is_error {
        return None;
    }
    let items = content.as_array()?;
    if items.len() != 1 || items[0]["type"] != "text" {
        return None;
    }
    let state: UiState = serde_json::from_str(items[0]["text"].as_str()?).ok()?;
    let UiState::Emotion { frame_ms, frames, .. } = &state else {
        return None;
    };
    if *frame_ms == 0 || frames.is_empty() {
        return None;
    }
    let duration = frame_ms.checked_mul(u64::try_from(frames.len()).ok()?)?;
    let deadline = Instant::now().checked_add(Duration::from_millis(duration))?;
    Some((state, deadline))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn content(state: &Value) -> Value {
        json!([{"type":"text","text":state.to_string()}])
    }

    #[tokio::test(start_paused = true)]
    async fn m6_decoder_accepts_only_actual_successful_typed_emotion_with_checked_duration() {
        let state = json!({"state":"emotion","name":"happy","frame_ms":80,"frames":["(^_^)","(^o^)"]});
        let expected = (
            serde_json::from_value(state.clone()).unwrap(),
            Instant::now() + Duration::from_millis(160),
        );
        assert_eq!(decode("ui.show_emotion", &content(&state), false), Some(expected));
        assert_eq!(decode("test.echo", &content(&state), false), None);
        assert_eq!(decode("ui.show_emotion", &content(&state), true), None);
        for invalid in [
            json!(null),
            json!([]),
            json!([{"type":"image","text":state.to_string()}]),
            json!([{"type":"text","text":"{"}]),
            json!([{"type":"text","text":"happy"}]),
            content(&json!({"state":"listening"})),
            content(&json!({"state":"future"})),
        ] {
            assert_eq!(decode("ui.show_emotion", &invalid, false), None);
        }
        for duration in [json!(0), json!(-1), json!(1.5), json!("80"), json!(u64::MAX)] {
            let mut invalid = state.clone();
            invalid["frame_ms"] = duration;
            assert_eq!(decode("ui.show_emotion", &content(&invalid), false), None);
        }
        let mut empty = state;
        empty["frames"] = json!([]);
        assert_eq!(decode("ui.show_emotion", &content(&empty), false), None);
    }
}
