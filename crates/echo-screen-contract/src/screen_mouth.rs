//! Lip sync for a speaking character: optional, and separate from `ScreenFrame`.
//! The screen server sends each batch as a named `mouth` event, which consumes
//! no frame sequence number. A screen that does not know the event ignores it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The vowel a mouth shapes, or `none` when no vowel is clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MouthVowel {
    None,
    A,
    I,
    U,
    E,
    O,
}

/// The mouth at one instant of reply audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MouthSample {
    /// When the audio is heard, in milliseconds on the sender's clock.
    pub at_ms: u64,
    /// How far the mouth is open: 0 is closed, 100 fully open.
    pub open: u8,
    pub vowel: MouthVowel,
}

/// Consecutive samples, sent ahead of the audio they describe. The sender's
/// clock is monotonic with an arbitrary start: `sent_ms` is its reading when
/// the batch left, so a receiver learns the offset to its own clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MouthBatch {
    pub sent_ms: u64,
    pub samples: Vec<MouthSample>,
}

#[cfg(test)]
mod tests {
    use super::{MouthBatch, MouthSample, MouthVowel};

    #[test]
    fn batch_wire_is_stable_and_ignores_unknown_fields() {
        let batch = MouthBatch {
            sent_ms: 1200,
            samples: vec![
                MouthSample {
                    at_ms: 1400,
                    open: 62,
                    vowel: MouthVowel::A,
                },
                MouthSample {
                    at_ms: 1420,
                    open: 0,
                    vowel: MouthVowel::None,
                },
            ],
        };
        let wire = r#"{"sent_ms":1200,"samples":[{"at_ms":1400,"open":62,"vowel":"a"},{"at_ms":1420,"open":0,"vowel":"none"}]}"#;
        assert_eq!(serde_json::to_string(&batch).unwrap(), wire);
        let extended = r#"{"sent_ms":1200,"later":true,"samples":[{"at_ms":1400,"open":62,"vowel":"a","tilt":3},{"at_ms":1420,"open":0,"vowel":"none"}]}"#;
        assert_eq!(serde_json::from_str::<MouthBatch>(extended).unwrap(), batch);
    }

    #[test]
    fn samples_reject_unknown_vowels_and_openness_beyond_a_byte() {
        for sample in [
            r#"{"at_ms":1,"open":1,"vowel":"y"}"#,
            r#"{"at_ms":1,"open":256,"vowel":"a"}"#,
            r#"{"at_ms":-1,"open":1,"vowel":"a"}"#,
            r#"{"at_ms":1,"open":1}"#,
        ] {
            assert!(serde_json::from_str::<MouthSample>(sample).is_err(), "{sample}");
        }
    }
}
