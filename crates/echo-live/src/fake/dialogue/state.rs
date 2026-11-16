use echo_contracts::ids::DelegationId;
use echo_contracts::live::LiveEvent;
use serde_json::json;

use super::{AudioFrame, CallError, DialogueScript, DialogueSent};

impl DialogueScript {
    pub(super) fn validate(&self) -> Result<(), CallError> {
        if self.delegation_id.len() > 128
            || DelegationId::new(self.delegation_id.clone()).is_err()
            || self.initial_after_audio_frames > 128
            || !(1..=20).contains(&self.audio_frames)
            || [&self.input, &self.commentary, &self.transcript]
                .iter()
                .any(|text| text.is_empty() || text.len() > 2048)
        {
            return Err(CallError::Configuration);
        }
        Ok(())
    }
}

pub(super) struct Dialogue {
    script: DialogueScript,
    replied: bool,
    messages: u16,
    rejected: u16,
    microphone_frames: u16,
}

impl Dialogue {
    pub(super) fn new(script: DialogueScript) -> Self {
        Self {
            script,
            replied: false,
            messages: 0,
            rejected: 0,
            microphone_frames: 0,
        }
    }

    pub(super) fn rejected(&self) -> u16 {
        self.rejected
    }

    pub(super) fn commentary(&mut self, text: &str) -> Result<bool, CallError> {
        if self.messages == 64 || text.len() > 16_384 {
            return Err(CallError::Decode);
        }
        self.messages += 1;
        if !self.replied
            && let Ok(LiveEvent::CommentaryAppend {
                delegation_id, text, ..
            }) = crate::call::decode(text)
            && delegation_id.to_string() == self.script.delegation_id
            && text == self.script.commentary
        {
            self.replied = true;
            return Ok(true);
        }
        self.rejected += 1;
        Ok(false)
    }

    pub(super) fn microphone(&mut self) -> Result<(), CallError> {
        if self.microphone_frames == 128 {
            return Err(CallError::Decode);
        }
        self.microphone_frames += 1;
        Ok(())
    }

    pub(super) fn reply(&self) -> Vec<DialogueSent> {
        let mut reply = vec![DialogueSent::Text(
            json!({
                "type":"response.audio_transcript.done", "transcript":self.script.transcript
            })
            .to_string(),
        )];
        for sequence in 0..self.script.audio_frames {
            reply.push(DialogueSent::Audio(AudioFrame {
                sequence,
                timestamp: u32::from(sequence) * 960,
                // RFC 6716 comfort noise: valid 20-ms Opus, never microphone echo.
                payload: vec![0xf8, 0xff, 0xfe],
            }));
        }
        reply.push(DialogueSent::Text(json!({"type":"response.audio.done"}).to_string()));
        reply
    }
}
