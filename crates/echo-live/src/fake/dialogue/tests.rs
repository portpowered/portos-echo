use serde_json::json;

use super::*;
use state::Dialogue;

mod runtime;

fn script() -> DialogueScript {
    DialogueScript {
        delegation_id: "d-1".into(),
        input: "call the test tool".into(),
        commentary: "test echo: delegated echo".into(),
        transcript: "test echo: delegated echo".into(),
        audio_frames: 3,
    }
}

fn commentary(id: &str, text: &str) -> String {
    json!({"type":"session.commentary.append", "delegation_id":id,"text":text}).to_string()
}

#[test]
fn dialogue_requires_exact_identity_and_text_and_replies_once() {
    let mut dialogue = Dialogue::new(script());
    for invalid in [
        "not json".into(),
        "{}".into(),
        commentary("d-2", &script().commentary),
        commentary("d-1", "wrong text"),
        json!({"type":"session.commentary.append", "delegation_id":"d-1"}).to_string(),
    ] {
        assert_eq!(dialogue.commentary(&invalid), Ok(false));
    }
    assert_eq!(dialogue.rejected(), 5);
    assert_eq!(dialogue.commentary(&commentary("d-1", &script().commentary)), Ok(true));
    assert_eq!(dialogue.commentary(&commentary("d-1", &script().commentary)), Ok(false));
    assert_eq!(dialogue.rejected(), 6);
    let reply = dialogue.reply();
    assert_eq!(reply.len(), 5);
    assert_eq!(
        reply[0],
        DialogueSent::Text(
            json!({
                "type":"response.audio_transcript.done", "transcript":"test echo: delegated echo"
            })
            .to_string()
        )
    );
    for sequence in 0..3_u16 {
        assert_eq!(
            reply[usize::from(sequence) + 1],
            DialogueSent::Audio(AudioFrame {
                sequence,
                timestamp: u32::from(sequence) * 960,
                payload: vec![0xf8, 0xff, 0xfe]
            })
        );
    }
    assert_eq!(
        reply[4],
        DialogueSent::Text(json!({"type":"response.audio.done"}).to_string())
    );
}

#[test]
fn dialogue_bounds_messages_microphone_and_configuration() {
    let mut dialogue = Dialogue::new(script());
    for _ in 0..64 {
        assert_eq!(dialogue.commentary("{}"), Ok(false));
    }
    assert_eq!(dialogue.commentary("{}"), Err(CallError::Decode));
    assert_eq!(dialogue.rejected(), 64);
    for _ in 0..128 {
        assert_eq!(dialogue.microphone(), Ok(()));
    }
    assert_eq!(dialogue.microphone(), Err(CallError::Decode));
    assert_eq!(
        Dialogue::new(script()).commentary(&"x".repeat(16_385)),
        Err(CallError::Decode)
    );
    for field in 0..6 {
        let mut invalid = script();
        match field {
            0 => invalid.delegation_id.clear(),
            1 => invalid.delegation_id = "x".repeat(129),
            2 => invalid.input.clear(),
            3 => invalid.commentary = "x".repeat(2049),
            4 => invalid.transcript.clear(),
            _ => invalid.audio_frames = 21,
        }
        assert_eq!(invalid.validate(), Err(CallError::Configuration));
    }
    let mut valid = script();
    valid.audio_frames = 0;
    assert_eq!(valid.validate(), Err(CallError::Configuration));
    valid.audio_frames = 20;
    assert_eq!(valid.validate(), Ok(()));
}

#[test]
fn dialogue_error_reply_is_explicit_and_observations_remain_bounded() {
    let mut script = script();
    script.commentary = "I couldn't call test echo.".into();
    script.transcript = script.commentary.clone();
    let mut dialogue = Dialogue::new(script.clone());
    assert_eq!(
        dialogue.commentary(&commentary("d-1", "test echo: delegated echo")),
        Ok(false)
    );
    assert_eq!(dialogue.commentary(&commentary("d-1", &script.commentary)), Ok(true));
    assert_eq!(
        dialogue.reply()[0],
        DialogueSent::Text(
            json!({
                "type":"response.audio_transcript.done", "transcript":"I couldn't call test echo."
            })
            .to_string()
        )
    );
    let observer = DialogueObserver::default();
    for index in 0..MAX_CALLS {
        assert_eq!(observer.record("d-1"), Ok(index));
    }
    assert_eq!(observer.record("d-1"), Err(CallError::Signaling));
    assert_eq!(observer.calls().unwrap().len(), MAX_CALLS);
    assert_eq!(observer.update(MAX_CALLS, |_| ()), Err(CallError::Signaling));
}
