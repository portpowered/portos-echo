//! The provisional hold belongs to one reply of the admitted built-in voice call.
use super::Source;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum GoodbyeHold {
    #[default]
    None,
    Reply,
    NextReply,
}

impl Source {
    pub(in crate::bridge_voice) fn holds_goodbye(&self) -> bool {
        self.goodbye_hold == GoodbyeHold::Reply
    }

    pub(in crate::bridge_voice) fn clear_goodbye_hold(&mut self) -> bool {
        let held = self.holds_goodbye();
        if held {
            self.goodbye_hold = GoodbyeHold::None;
        }
        held
    }

    pub(super) fn consider_goodbye(&mut self, said: &str) {
        if !self.built_in_voice || self.session.listen().is_none() || self.goodbye_hold == GoodbyeHold::Reply {
            return;
        }
        self.goodbye_hold = if !standalone_goodbye(said) {
            GoodbyeHold::None
        } else if self.delegation.in_flight() || self.stale() || self.reply.finished() || self.barge.pending() {
            // An earlier reply already owns playback. Its completion stays
            // independent of a farewell the provider has yet to emit.
            GoodbyeHold::NextReply
        } else {
            GoodbyeHold::Reply
        };
    }
}

fn standalone_goodbye(text: &str) -> bool {
    let text = text.trim().trim_end_matches(['.', '!']).trim_end();
    let text = text.to_ascii_lowercase().replace('’', "'");
    matches!(
        text.as_str(),
        "goodbye"
            | "bye"
            | "thanks, that's all"
            | "thanks that's all"
            | "okay thanks, bye"
            | "okay, thanks, bye"
            | "okay thanks bye"
    )
}
