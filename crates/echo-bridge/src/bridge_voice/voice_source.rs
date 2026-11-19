use super::{Error, Stats, VoicePort, session_controller::Controller};
use crate::bridge_codex::{CaptureAudio, ReplyAudio};
use echo_link::MicFrame;
use echo_media::PcmFrame;
use std::collections::VecDeque;
use tokio::time::{Duration, Instant};

/// Samples at or below this level are WebRTC silence, not speech.
const QUIET: u16 = 64;
/// Thirty seconds of 20 ms frames: above the setup capture's own bound.
const UPLINK_BACKLOG: usize = 1500;

pub(super) struct Source {
    pub microphone: Option<CaptureAudio>,
    pub muted: bool,
    pub reply: ReplyAudio,
    pub controller: Controller,
    pub failure: &'static str,
    lead_ms: u16,
    pub delegation: Delegation,
    pub mouth: Mouth,
    pub stats: Stats,
    pub session: super::session_tools::Grants,
    /// Speech the bounded media sender has not taken yet, oldest first.
    uplink: VecDeque<PcmFrame>,
}

impl Source {
    pub fn resume_reply(&mut self) -> Result<(), Error> {
        self.controller.resume()?;
        self.stats.reply_dropped += self.reply.dropped_frames();
        self.reply = ReplyAudio::new(self.lead_ms)?;
        self.delegation = Delegation::default();
        self.mouth = Mouth::default();
        Ok(())
    }

    pub fn new(lead_ms: u16, muted: bool) -> Result<Self, Error> {
        Ok(Self {
            microphone: None,
            muted,
            reply: ReplyAudio::new(lead_ms)?,
            controller: Controller::default(),
            failure: "playback",
            lead_ms,
            delegation: Delegation::default(),
            mouth: Mouth::default(),
            stats: Stats::default(),
            session: super::session_tools::Grants::default(),
            uplink: VecDeque::new(),
        })
    }

    /// Retire all old playback state together; never flush an abandoned tail.
    /// Stream order identifies the replacement at its next assistant delta.
    pub fn interrupt(&mut self, port: &mut impl VoicePort) -> Result<(), Error> {
        port.interrupt_turn()?;
        if !self.controller.interruptible(self.delegation.turns > 0) {
            return Ok(());
        }
        if self.controller.speaking() {
            port.retire_audio();
        }
        self.stats.reply_dropped += self.reply.dropped_frames();
        self.reply = ReplyAudio::new(self.lead_ms)?;
        self.delegation = Delegation::default();
        self.mouth = Mouth::default();
        self.controller.cut();
        Ok(())
    }

    /// Return whether this chunk was suppressed by the defensive mute gate.
    /// Adapter-suppressed chunks arrive separately as ordered device events.
    pub fn capture(&mut self, frame: Option<MicFrame>, port: &impl VoicePort) -> Result<bool, Error> {
        let Some(frame) = frame else {
            self.controller.microphone_ended();
            if let Some(microphone) = &mut self.microphone {
                let frames = microphone.finish()?;
                self.stats.microphone_tail = microphone.tail_samples();
                for frame in frames {
                    self.forward(&frame, port)?;
                }
            }
            return Ok(false);
        };
        self.stats.microphone_chunks += 1;
        if self.muted {
            return Ok(true);
        }
        if self.microphone.is_none() {
            self.microphone = Some(CaptureAudio::new(frame.format)?);
        }
        let microphone = self.microphone.as_mut().ok_or(Error::Audio)?;
        for frame in microphone.process(&frame.format, &frame.payload)? {
            self.forward(&frame, port)?;
        }
        Ok(false)
    }

    /// Speech captured while the call was set up is replayed as one burst, far
    /// faster than the bounded media sender encodes it. Keep what the sender
    /// refuses, in order, instead of letting it evict the start of the request.
    fn forward(&mut self, frame: &PcmFrame, port: &impl VoicePort) -> Result<(), Error> {
        self.flush_uplink(port)?;
        if !self.uplink.is_empty() || !port.try_send(*frame)? {
            if self.uplink.len() == UPLINK_BACKLOG {
                self.uplink.pop_front();
                self.stats.uplink_dropped += 1;
            }
            self.uplink.push_back(*frame);
        }
        self.stats.microphone_frames += 1;
        Ok(())
    }

    /// Hand the oldest waiting speech to the sender until it is full again.
    pub fn flush_uplink(&mut self, port: &impl VoicePort) -> Result<(), Error> {
        while let Some(frame) = self.uplink.front() {
            if !port.try_send(*frame)? {
                break;
            }
            self.uplink.pop_front();
        }
        Ok(())
    }

    #[must_use]
    pub fn uplink_pending(&self) -> bool {
        !self.uplink.is_empty()
    }

    /// Mute withholds everything not yet handed to the media sender.
    pub fn mute_uplink(&mut self) {
        self.microphone = None;
        self.uplink.clear();
    }

    /// Continuous RTP received after a completed reply belongs to the retired
    /// converter. Discard both quiet and audible media at that boundary.
    pub fn audio(&mut self, frame: &PcmFrame) -> Result<(), Error> {
        if self.controller.accepts_audio() && !self.reply.finished() && !self.delegation.abandoned {
            // PCM has no utterance identity. Only ordered assistant control
            // opens Speaking; the initial user transcript can lag audible RTP.
            self.reply.push(frame)?;
        }
        self.stats.reply_received += 1;
        Ok(())
    }
}

/// Preserve playback ownership until the device acknowledges an interrupted
/// reply. A cancellation cannot safely resume the connection after a timeout.
pub(super) async fn settle_cut(
    playback: impl std::future::Future<Output = Result<(), Error>>,
    result: &std::cell::Cell<Option<Result<echo_link::PlayOutcome, echo_link::Error>>>,
    deadline: std::time::Duration,
) -> Result<(), Error> {
    if result.get().is_none() {
        // Cut maps to Wyoming in the ordinary successful-reply path. Here the
        // recorded outcome distinguishes an acknowledged cut from transport loss.
        let _ = tokio::time::timeout(deadline, playback)
            .await
            .map_err(|_| Error::Deadline)?;
    }
    match result.get() {
        Some(Ok(_)) => Ok(()),
        Some(Err(error)) => Err(error.into()),
        None => Err(Error::Wyoming),
    }
}

/// Failure-only, closed-vocabulary diagnostics: never include device frames,
/// provider text, addresses or credentials. Preserve the original failure.
pub(super) async fn cut_generation(
    client: &impl echo_link::EchoLink,
    playback: impl std::future::Future<Output = Result<(), Error>>,
    result: &std::cell::Cell<Option<Result<echo_link::PlayOutcome, echo_link::Error>>>,
    deadline: std::time::Duration,
    source: &Source,
    sink_open: bool,
) -> Result<(), Error> {
    let cut = tokio::time::timeout(std::time::Duration::from_secs(1), client.cut_with_stats()).await;
    let settled = async {
        let stats = cut.map_err(|_| Error::Deadline)??;
        // The cut discarded reply frames the paused clock counted as accepted.
        if let Some(clock) = crate::bridge_mcp::bridge_clock::receipt::Context::current() {
            clock.discarded("reply", stats.dropped_frames)?;
        }
        settle_cut(playback, result, deadline).await
    }
    .await;
    if let Err(error) = &settled {
        playback_failure("cut_ack", error, source, sink_open, result.get());
    }
    settled
}

pub(super) fn playback_failure(
    stage: &'static str,
    error: &Error,
    source: &Source,
    sink_open: bool,
    playback: Option<Result<echo_link::PlayOutcome, echo_link::Error>>,
) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr(),
        "echo-bridge: wyoming stage={stage} error={error:?} state={:?} sink_open={sink_open} finished={} drained={} played={} cut_receipts={} playback={playback:?}",
        source.controller,
        source.reply.finished(),
        source.reply.drained(),
        source.controller.has_played(),
        source.controller.pending_cut_receipts(),
    );
}

/// Delegated Codex turns and the assistant utterance they bracket. Each
/// delegated turn has at most one filler ("one sec"): the utterance in progress
/// when the turn starts, or else the first one begun while it runs. A filler's
/// done holds the reply open; any later utterance is the answer, even when it
/// begins before turn/completed arrives. A filler still speaking after its turn
/// completed has run on into the answer, so its done ends the reply. The order
/// of the filler's done, the answer's deltas and turn/completed varies by call.
/// The assistant utterance in progress, by its role in a delegated turn.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Utterance {
    #[default]
    None,
    Filler,
    /// A filler whose delegated turn completed before its done.
    Settled,
    Answer,
}

#[derive(Default)]
pub struct Delegation {
    turns: usize,
    armed: bool,
    utterance: Utterance,
    held: bool,
    abandoned: bool,
}

impl Delegation {
    pub fn started(&mut self) {
        self.turns += 1;
        if self.utterance == Utterance::Answer {
            self.utterance = Utterance::Filler;
        } else {
            self.armed = true;
        }
    }

    pub fn completed(&mut self) {
        self.turns = self.turns.saturating_sub(1);
        if self.turns == 0 {
            self.armed = false;
            if self.utterance == Utterance::Filler {
                self.utterance = Utterance::Settled;
            }
        }
    }

    pub fn spoke(&mut self) {
        self.utterance = match self.utterance {
            Utterance::None if self.armed && self.turns > 0 => {
                self.armed = false;
                Utterance::Filler
            }
            Utterance::None | Utterance::Settled => Utterance::Answer,
            utterance => utterance,
        };
    }

    /// True when this assistant done ends the reply rather than a filler.
    pub fn answered(&mut self) -> bool {
        let utterance = std::mem::take(&mut self.utterance);
        self.held = matches!(utterance, Utterance::Filler | Utterance::Settled);
        !self.held
    }

    /// A filler done is holding the reply open for a delegated answer.
    #[must_use]
    pub fn held(&self) -> bool {
        self.held && !self.abandoned
    }

    /// The response deadline passed first: the reply ends after the filler and
    /// the answer, if it still comes, is dropped instead of failing the run.
    pub fn abandon(&mut self) {
        self.abandoned = true;
    }
}

const FRAME: Duration = Duration::from_millis(20);
/// Measured on the real Echo: hassmic reports `played` 0.07 to 0.24 s (typically
/// 0.22 s) after the modelled playout clock below drains.
const OUTPUT_DELAY: Duration = Duration::from_millis(200);

/// The screen's mouth follows audio the Echo is actually playing, not model
/// events. Frames written to the Echo play back to back on its clock, which
/// restarts when the device ran dry. The mouth opens when the first audible
/// frame reaches the speaker and closes when the last one has played.
#[derive(Default)]
pub struct Mouth {
    clock: Option<Instant>,
    transitions: VecDeque<(Instant, bool)>,
}

impl Mouth {
    /// Record one 24 kHz s16le frame written to the Echo at `now`.
    pub fn wrote(&mut self, frame: &[u8], now: Instant) {
        if frame.is_empty() {
            return;
        }
        let start = self.clock.filter(|end| *end > now).unwrap_or(now);
        self.clock = Some(start + FRAME);
        let audible = frame
            .as_chunks::<2>()
            .0
            .iter()
            .any(|sample| i16::from_le_bytes(*sample).unsigned_abs() > QUIET);
        if audible {
            let open = start + OUTPUT_DELAY;
            let close = open + FRAME;
            if let Some((due, false)) = self.transitions.back_mut()
                && *due == open
            {
                *due = close;
            } else {
                self.transitions.push_back((open, true));
                self.transitions.push_back((close, false));
            }
        }
    }

    /// When the next open or close is due.
    #[must_use]
    pub fn due(&self) -> Option<Instant> {
        self.transitions.front().map(|(due, _)| *due)
    }

    /// The change due by `now`, if any: true opens the mouth, false closes it.
    pub fn poll(&mut self, now: Instant) -> Option<bool> {
        self.due().filter(|due| *due <= now)?;
        self.transitions.pop_front().map(|(_, open)| open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(level: i16) -> Vec<u8> {
        level.to_le_bytes().repeat(480)
    }

    /// A bounded media sender that takes `room` more frames, then refuses.
    #[derive(Default)]
    struct Narrow {
        room: std::cell::Cell<usize>,
        sent: std::cell::RefCell<Vec<i16>>,
    }

    impl VoicePort for Narrow {
        fn send(&self, _frame: PcmFrame) -> Result<(), Error> {
            // The evicting sender is never an acceptable path for speech.
            Err(Error::Audio)
        }
        fn try_send(&self, frame: PcmFrame) -> Result<bool, Error> {
            if self.room.get() == 0 {
                return Ok(false);
            }
            self.room.set(self.room.get() - 1);
            self.sent.borrow_mut().push(frame[0]);
            Ok(true)
        }
        async fn next(&mut self) -> Result<crate::bridge_codex::MediaEvent, Error> {
            std::future::pending().await
        }
        fn ready_audio(&mut self) -> impl Future<Output = Result<Option<PcmFrame>, Error>> {
            std::future::ready(Ok(None))
        }
    }

    #[test]
    fn setup_burst_waits_for_the_sender_in_order_and_mute_withholds_the_rest() {
        let mut source = Source::new(200, false).unwrap();
        let port = Narrow::default();
        port.room.set(2);
        for marker in 1..=5 {
            source.forward(&[marker; 320], &port).unwrap();
        }
        assert_eq!(*port.sent.borrow(), [1, 2]);
        assert!(source.uplink_pending());
        assert_eq!(source.stats.microphone_frames, 5);
        port.room.set(2);
        source.flush_uplink(&port).unwrap();
        assert_eq!(*port.sent.borrow(), [1, 2, 3, 4]);
        // Room for one: waiting speech goes first, the new frame queues behind it.
        port.room.set(1);
        source.forward(&[6; 320], &port).unwrap();
        assert_eq!(*port.sent.borrow(), [1, 2, 3, 4, 5]);
        assert!(source.uplink_pending());
        source.mute_uplink();
        port.room.set(5);
        source.flush_uplink(&port).unwrap();
        assert_eq!(*port.sent.borrow(), [1, 2, 3, 4, 5]);
        assert!(!source.uplink_pending());
        assert_eq!(source.stats.microphone_frames, 6);
    }

    #[test]
    fn stalled_sender_backlog_is_bounded_and_keeps_the_newest_speech() {
        let mut source = Source::new(200, false).unwrap();
        let port = Narrow::default();
        for marker in 0..=UPLINK_BACKLOG {
            source.forward(&[i16::try_from(marker).unwrap(); 320], &port).unwrap();
        }
        port.room.set(usize::MAX);
        source.flush_uplink(&port).unwrap();
        let sent = port.sent.borrow();
        assert_eq!(sent.len(), UPLINK_BACKLOG);
        assert_eq!((sent[0], sent[UPLINK_BACKLOG - 1]), (1, 1500));
        assert_eq!(source.stats.uplink_dropped, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn mouth_opens_and_closes_on_the_echo_playout_clock_not_on_writes() {
        let mut mouth = Mouth::default();
        let t0 = Instant::now();
        // Silence first, then a lead burst of 5 audible frames written at t0.
        mouth.wrote(&frame(0), t0);
        for _ in 0..5 {
            mouth.wrote(&frame(QUIET.cast_signed() + 1), t0);
        }
        assert_eq!(mouth.due(), Some(t0 + FRAME + OUTPUT_DELAY));
        assert_eq!(mouth.poll(t0 + OUTPUT_DELAY), None);
        assert_eq!(mouth.poll(t0 + FRAME + OUTPUT_DELAY), Some(true));
        // Five audible frames after the leading silence finish at 120 ms.
        assert_eq!(mouth.due(), Some(t0 + FRAME * 6 + OUTPUT_DELAY));
        // Quiet frames keep the clock running but never reopen or extend it.
        mouth.wrote(&frame(-QUIET.cast_signed()), t0 + FRAME * 3);
        assert_eq!(mouth.poll(t0 + FRAME * 6 + OUTPUT_DELAY), Some(false));
        assert_eq!(mouth.due(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn mouth_restarts_the_clock_after_the_echo_ran_dry() {
        let mut mouth = Mouth::default();
        let t0 = Instant::now();
        mouth.wrote(&frame(1000), t0);
        assert_eq!(mouth.poll(t0 + OUTPUT_DELAY), Some(true));
        assert_eq!(mouth.poll(t0 + FRAME + OUTPUT_DELAY), Some(false));
        let later = t0 + Duration::from_secs(3);
        mouth.wrote(&frame(-1000), later);
        assert_eq!(mouth.due(), Some(later + OUTPUT_DELAY));
        assert_eq!(mouth.poll(later + OUTPUT_DELAY), Some(true));
    }

    #[tokio::test(start_paused = true)]
    async fn mouth_quiet_thresholds_never_schedule_a_transition() {
        let mut mouth = Mouth::default();
        let t0 = Instant::now();
        for level in [0, 64, -64] {
            mouth.wrote(&frame(level), t0);
        }
        mouth.wrote(&[], t0);
        assert_eq!(mouth.due(), None);
        assert_eq!(mouth.poll(t0 + Duration::from_secs(1)), None);
    }

    #[tokio::test(start_paused = true)]
    async fn mouth_preserves_quiet_gaps_even_when_all_audio_is_written_ahead() {
        let mut mouth = Mouth::default();
        let t0 = Instant::now();
        for level in [65, 0, -65] {
            mouth.wrote(&frame(level), t0);
        }
        for (offset, open) in [(0, true), (1, false), (2, true), (3, false)] {
            let due = t0 + OUTPUT_DELAY + FRAME * offset;
            assert_eq!(mouth.poll(due - Duration::from_nanos(1)), None);
            assert_eq!(mouth.poll(due), Some(open));
        }
        assert_eq!(mouth.due(), None);
    }
}
