//! Project pure session facts onto the existing structure-only trace rows.
use crate::bridge_voice::{Error, Notice, Source};
#[cfg(test)]
pub(crate) use echo_core_api::SPEECH_START_EXPIRY;
use echo_core_api::{SessionMark as Mark, SessionTurnEvent};
use serde_json::{Value, json};

pub(in crate::bridge_voice) trait MarkFields {
    fn event(&self) -> &'static str;
    fn fields(&self) -> Value;
}

impl MarkFields for Mark {
    fn event(&self) -> &'static str {
        match self {
            Self::Latched { .. } => "speech.latched",
            Self::Capped(_) => "run.capped",
            Self::Lapsed(_) => "run.lapsed",
            Self::BargeCut { .. } => "barge.cut",
            Self::BargePassed { .. } => "barge.suppressed_in_turn",
            Self::BargeRearmed { .. } => "barge.rearmed",
            Self::BargeLeft { .. } => "barge.model_only",
        }
    }

    fn fields(&self) -> Value {
        match *self {
            Self::Latched {
                held,
                reason,
                transcribed,
            } => json!({
                "held_ms": u64::try_from(held.as_millis()).unwrap_or(u64::MAX),
                "reason": reason,
                "transcribed": transcribed,
            }),
            Self::Capped(during) => json!({ "during": during }),
            Self::Lapsed(window) => json!({ "window": window }),
            Self::BargeCut { turn, trigger } | Self::BargeLeft { turn, trigger } => {
                json!({ "turn": turn, "trigger": trigger.name() })
            }
            Self::BargePassed { turn, trigger, count } => {
                json!({ "turn": turn, "trigger": trigger.name(), "count": count })
            }
            Self::BargeRearmed { turn, reason } => json!({ "turn": turn, "reason": reason }),
        }
    }
}

/// Hand the run's pending marks to its observer, oldest first.
pub(in crate::bridge_voice) fn report(
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    for mark in source.controller.take_marks() {
        observe(Notice::Mark(mark))?;
    }
    Ok(())
}

pub(in crate::bridge_voice) fn run_ended(
    source: &mut Source,
    observe: &mut impl FnMut(Notice) -> Result<(), Error>,
) -> Result<(), Error> {
    source.controller.turn(SessionTurnEvent::RunEnded);
    report(source, observe)
}
