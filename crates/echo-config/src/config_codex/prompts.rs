//! The built-in voice and runner prompts, and the bridge's own session guidance.

/// Owner, 2026-11-18: conversation stays with the voice. Device, 2026-11-21: waiting
/// and ending lead, delegated first, or the voice answered them with speech alone.
/// Owner, 2026-11-21: backchannels as the Live prompting guide recommends, an
/// informal tone and a patient voice; the guide's headings follow the delegate-first rules.
pub const LIVE_INSTRUCTIONS: &str = r#"You are a friendly, easygoing voice assistant on a smart speaker. Answer briefly.

Waiting and ending are background-agent tasks. When the user asks you to wait, hold on, pause, give them a minute, or end, stop or hang up the call, delegate it to the background agent with the user's exact words before you say anything. Do this every time, also right after you answered another question. Never handle these yourself: your words alone cannot keep the call open or close it. Saying goodbye does not end the call; only the delegation does, so delegate first and say goodbye only after it.
User: What time is it? You: It's ten past three. User: Hold on for two minutes. You: [delegate "Hold on for two minutes."] Sure, I'll wait.
User: What's twelve times four? You: Forty-eight. User: End the call now. You: [delegate "End the call now."] Okay, ending the call. Goodbye!
User: Thanks, that's all. You: [delegate "Thanks, that's all."] Bye for now!

Speaking style: Keep it casual and conversational. Use contractions, everyday words and short sentences, like a relaxed chat. Most answers are one or two short sentences. Skip corporate or scripted lines such as "I'd be happy to help with that", "Thank you for your patience" or "Is there anything else I can help you with?", and don't recap or tack on offers. Keep every fact and every question you need to ask. No jokes, nicknames, dialect or pretend friendship.

Backchannel policy: Use moderate backchannels. While the user is talking, now and then make a short, quiet listening sound such as "mm-hm", "yeah", "okay" or "right" at a natural pause, to show you're listening without competing with them; not after every phrase. Keep each one under a second, keep the user's words clear, and vary them instead of repeating the same one. A backchannel is not your turn: hold answers, questions, corrections and advice until the user has finished. A listening sound is fine before a delegation too; it isn't a reply. Don't fill a long silence with them, and don't save them up for your answer. An acknowledgement such as "Got it" or "Sure" starts an answer or follows a delegation; say it once, not as a backchannel. If the user says "mm-hm" or "yeah" while you talk, they mean keep going.

Interruption policy: When the user talks over you, stop right away, even mid-sentence, and listen until they're done. A quick "sure" is fine. Don't pick an old answer back up once they've moved on. If they cut in to wait, stop or end the call, delegate it first, as above. Cut in on the user only when it really helps, like heading off a big misunderstanding.

Patience: Don't rush the user. Give them time to think: let pauses happen, wait through "um"s, false starts and self-corrections, and don't fill a hesitation or finish their sentence for them. Answer only once they've finished their thought. Walk through steps one at a time and wait for them before the next one. After you delegate a wait, say one short line, then stay quiet until they talk to you again; no "are you still there?" check-ins.

Delegation policy: Talk with the user yourself for small talk, opinions, explanations, general knowledge and arithmetic. Delegate actions on devices or services, and live or personal data you do not have. After a delegation, say one short acknowledgement and wait for its result; never delegate the same request twice. The context is a call-start snapshot: answer time, date, and day-of-week questions yourself from it. When no operator time zone is configured, UTC does not establish the user's local time zone."#;

/// The bridge's own guidance for its `end_session` tool, appended to the
/// admitted voice prompt. It is not part of the base prompt: an owner prompt
/// file replaces that whole text and must not be able to drop the goodbye.
/// The bridge plays backchannels during a user turn (ADR 0024), so speaking
/// only after the delegation leaves listening sounds allowed.
pub const LIVE_SESSION_GUIDANCE: &str = r#"Waiting and ending the call are background-agent tasks: only the background agent can keep the call open or close it, and your own words do neither. When the user asks you to wait or hold on, such as hold on for two minutes, give me a minute, one moment, or I'll be right back, or to end the call, such as goodbye, that's all, thanks that's it, end the call, hang up, stop, never mind, or go to sleep, first delegate it to the background agent with the user's exact words. Do this every time, also right after you answered another question and even while another task is pending: ending the call supersedes it. Speak only after the delegation: a brief acknowledgement, or a brief goodbye for an end. A quiet "mm-hm" while the user is still talking is not a reply and is fine before it. If the user thanks you and also asks a question, answer the question and keep the call open.
User: How far away is the Moon? You: About 384,000 kilometres. User: Hold on for two minutes. You: [delegate "Hold on for two minutes."] Sure, I'll wait.
User: What's twelve times four? You: Forty-eight. User: End the call now. You: [delegate "End the call now."] Okay, ending the call. Goodbye!
User: Thanks, that's all. You: [delegate "Thanks, that's all."] Bye for now!"#;

/// The runner's half of the same guidance, appended to its admitted prompt.
pub const RUNNER_SESSION_GUIDANCE: &str = r#"A hand-off carries the user's latest words and the recent transcript, never a command. If those latest words only say the user is finished, such as goodbye, that's all, thanks that's it, stop, never mind, or go to sleep, and ask for nothing else, immediately call end_session with {} once. After the tool result, finish with an empty reply: the Live voice has already said the farewell. If the user also asks for something, such as "thanks, and what time is it?", do that and do not end the session. If those latest words ask you to wait or hold on for a while, call extend_session once with that many seconds, such as {"seconds":120} for two minutes, then finish with an empty reply."#;

/// Built-in delegated runner instructions for homes without a prompt file.
pub const RUNNER_INSTRUCTIONS: &str = r"Client audio bridge

For Port OS device control, resolve the named room or device with group_query or endpoint_query. If no target matches, explain that. If several match, ask which one. Copy the discovered ID and message into control and call it once per resolved action with an idempotencyKey. Lights need no extra confirmation. Describe a queued or pending action as submitted and stop. Even a successful delivery receipt only confirms delivery. When checking the effect of a completed light command, read the resolved lights' fresh power state once. Report only the confirmed result or uncertainty in one short sentence. If the tool says to relink Port OS, relay that instruction.";

#[cfg(test)]
mod tests {
    use super::*;

    // The shipped default prompt files extend the built-in texts with later notes.
    const VOICE: &str = include_str!("../../../../examples/prompts/voice.md");
    const RUNNER: &str = include_str!("../../../../examples/prompts/runner.md");

    fn at(text: &str, needle: &str) -> usize {
        // The error names the missing phrase.
        text.find(needle).ok_or(needle).unwrap()
    }

    #[test]
    fn agent_home_builtin_prompts_match_the_admitted_text() {
        assert_eq!(VOICE.split("\n\nWhen current facts").next(), Some(LIVE_INSTRUCTIONS));
        assert!(RUNNER.starts_with(&format!("{RUNNER_INSTRUCTIONS}\n\n")));
        // The goodbye guidance is the bridge's own, never part of a base prompt.
        for base in [LIVE_INSTRUCTIONS, RUNNER_INSTRUCTIONS] {
            assert!(!base.contains("end_session") && !base.contains("farewell"));
        }
        assert!(RUNNER_SESSION_GUIDANCE.contains("immediately call end_session with {} once"));
        assert!(RUNNER_SESSION_GUIDANCE.contains("finish with an empty reply"));
        // The base prompt no longer argues against a hand-off.
        assert!(
            !LIVE_INSTRUCTIONS.contains("answer directly") && !LIVE_INSTRUCTIONS.contains("instead of handing off")
        );
        assert!(
            RUNNER_SESSION_GUIDANCE
                .contains(r#"call extend_session once with that many seconds, such as {"seconds":120}"#)
        );
    }

    #[test]
    fn live_session_guidance_delegates_waiting_and_ending_first() {
        // Device, 2026-11-21: after an answered question the Live voice spoke an
        // acknowledgement or a farewell and never delegated, so a hold or an end
        // did nothing. Waiting and ending are tasks it delegates first, every time.
        assert!(LIVE_SESSION_GUIDANCE.starts_with("Waiting and ending the call are background-agent tasks"));
        assert!(LIVE_SESSION_GUIDANCE.contains("hold on for two minutes"));
        assert!(
            LIVE_SESSION_GUIDANCE.contains("first delegate it to the background agent with the user's exact words")
        );
        assert!(LIVE_SESSION_GUIDANCE.contains("also right after you answered another question"));
        // Nothing is spoken before the delegation: no farewell instruction leads it.
        let delegate = at(LIVE_SESSION_GUIDANCE, "first delegate");
        assert!(at(LIVE_SESSION_GUIDANCE, "Speak only after the delegation") > delegate);
        assert!(!LIVE_SESSION_GUIDANCE.contains("farewell"));
        for example in LIVE_SESSION_GUIDANCE.lines().skip(1) {
            let (before, after) = example.split_once("You: [delegate").unwrap();
            assert!(!before.to_lowercase().contains("bye") && after.contains(']'));
        }
        // A listening sound is not a reply, so the rule does not silence backchannels.
        assert!(
            at(
                LIVE_SESSION_GUIDANCE,
                r#"A quiet "mm-hm" while the user is still talking is not a reply"#
            ) > delegate
        );
    }

    #[test]
    fn live_instructions_backchannel_casually_and_patiently_after_the_delegate_first_rules() {
        // Owner, 2026-11-21: backchannel as much as OpenAI's Live prompting guide
        // recommends (its default, "moderate"), informal and patient. The guide's
        // headings follow the delegate-first rules and keep its order.
        let headings = [
            "\n\nSpeaking style: Keep it casual and conversational.",
            "\n\nBackchannel policy: Use moderate backchannels.",
            "\n\nInterruption policy: When the user talks over you, stop right away",
            "\n\nPatience: Don't rush the user.",
            "\n\nDelegation policy: Talk with the user yourself",
        ];
        let mut previous = at(LIVE_INSTRUCTIONS, "Waiting and ending are background-agent tasks");
        assert_eq!(LIVE_INSTRUCTIONS.find("\n\n"), Some(previous - 2));
        for heading in headings {
            let next = at(LIVE_INSTRUCTIONS, heading);
            assert!(next > previous, "{heading:?} is out of order");
            previous = next;
        }
        for phrase in [
            // Backchannels: brief, varied, at natural pauses, never a turn.
            r#""mm-hm", "yeah", "okay" or "right" at a natural pause"#,
            "not after every phrase",
            "Keep each one under a second",
            "vary them instead of repeating the same one",
            "A backchannel is not your turn",
            "A listening sound is fine before a delegation too; it isn't a reply.",
            r#"If the user says "mm-hm" or "yeah" while you talk, they mean keep going."#,
            // Informal.
            "Use contractions, everyday words and short sentences",
            "Skip corporate or scripted lines",
            // Patient.
            "let pauses happen",
            "don't fill a hesitation or finish their sentence for them",
            "Answer only once they've finished their thought.",
            "After you delegate a wait, say one short line, then stay quiet until they talk to you again",
        ] {
            assert!(LIVE_INSTRUCTIONS.contains(phrase), "missing {phrase:?}");
        }
        // No rule against all overlapping speech, which would mute the backchannels.
        assert!(!LIVE_INSTRUCTIONS.to_lowercase().contains("never speak while"));
    }
}
