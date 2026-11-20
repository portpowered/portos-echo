//! The reply's own echo taken out of the microphone before it goes upstream.
//! This measures and cancels; it never judges whether somebody is speaking.
use crate::Error;
use echo_audio::{ECHO_FRAME, EchoCanceller, EchoFrame, PcmResampler};
use echo_contracts::wyoming::AudioFormat;
use echo_media::PcmFrame;
use std::collections::VecDeque;
use std::time::Duration;
use tokio::time::Instant;

/// One canceller frame.
const PERIOD: Duration = Duration::from_millis(10);
/// One reply frame as it is written to the Echo, and one microphone frame.
const WRITTEN: Duration = Duration::from_millis(20);
/// Two seconds of reply not yet due at the microphone: more than the Echo
/// is ever written ahead, and a bound while the microphone is muted.
const BACKLOG: usize = 200;
/// hassmic sends 64 ms of microphone at once and a network holds some back:
/// a frame up to this long behind its arrival is in its place.
const LATE: Duration = Duration::from_millis(100);
/// A burst after a stall ends on time. This many frames behind in a row, or
/// one frame this far behind, were not delayed: audio between was lost.
const LOST: u32 = 50;
const BEHIND: Duration = Duration::from_secs(1);

/// What the canceller measured under one reply, for `reply.quality`. Small
/// on purpose: it rides in every settled reply's statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled {
    /// AEC3's estimate of the delay from a reply frame's modelled playout to
    /// its echo at the microphone; 0 before any reply audio.
    pub delay_ms: u16,
    /// Echo return loss enhancement in tenths of a decibel: the energy of
    /// the microphone while reply audio could be heard, before the canceller
    /// over after it. Speech from the room in those frames lowers it.
    pub erle_db10: i16,
    /// The short-term reduction reached 15 dB under this reply.
    pub converged: bool,
}

/// WebRTC's AEC3 between the reply written to the Echo and its microphone.
/// AEC3 wants one far-end frame for each microphone frame, so this keeps
/// the reply on the Echo's modelled playout clock (frames play back to back
/// from the first write, as `Ear` and `Mouth` model it) and hands over the
/// frame due as each microphone frame is taken. The speaker's delay, the
/// room and the way back are then one steady delay, which AEC3 finds.
///
/// Off, and until the run's first reply frame, microphone audio passes
/// untouched: there is nothing to cancel, and a request is never filtered.
#[derive(Default)]
pub(crate) struct Canceller {
    state: Option<Box<State>>,
}

struct State {
    aec: EchoCanceller,
    resampler: PcmResampler,
    scratch: Vec<i16>,
    /// Resampled reply short of one whole frame.
    partial: Vec<i16>,
    /// Far-end frames by the start of their playout, oldest first.
    far: VecDeque<(Instant, EchoFrame)>,
    /// Where the Echo's playout ends; none before the first reply frame.
    playout: Option<Instant>,
    /// The playout start of the next far-end frame.
    next: Instant,
    /// The capture time of the next microphone frame, and how many frames
    /// in a row have come late for it.
    microphone: Option<Instant>,
    late: u32,
    failed: bool,
}

impl Canceller {
    /// `delay_hint_ms` is where AEC3 looks for the echo first; none is off.
    /// Off as well under the sim's stepped clock, which stands still while
    /// the device plays: no far-end frame would ever come due there.
    pub(crate) fn new(delay_hint_ms: Option<u16>) -> Result<Self, Error> {
        let stepped = crate::bridge_mcp::bridge_clock::receipt::Context::current().is_some();
        let Some(hint) = delay_hint_ms.filter(|_| !stepped) else {
            return Ok(Self::default());
        };
        let written = AudioFormat {
            rate: 24_000,
            width: 2,
            channels: 1,
            extra: std::collections::BTreeMap::default(),
        };
        let resampler = PcmResampler::new(&written, echo_audio::ECHO_RATE)?;
        Ok(Self {
            state: Some(Box::new(State {
                aec: EchoCanceller::new(hint),
                scratch: vec![0; resampler.output_capacity()],
                resampler,
                partial: Vec::new(),
                far: VecDeque::new(),
                playout: None,
                next: Instant::now(),
                microphone: None,
                late: 0,
                failed: false,
            })),
        })
    }

    /// One signed 16-bit little-endian 24 kHz frame was written to the Echo.
    pub(crate) fn played(&mut self, frame: &[u8], now: Instant) {
        if let Some(state) = &mut self.state {
            state.failed |= state.played(frame, now).is_err();
        }
    }

    /// One microphone frame taken at `now`, without the reply's echo.
    /// # Errors
    /// Reports a frame the resampler or the canceller refused.
    pub(crate) fn cleaned(&mut self, frame: &PcmFrame, now: Instant) -> Result<PcmFrame, Error> {
        match &mut self.state {
            Some(state) => state.cleaned(frame, now),
            None => Ok(*frame),
        }
    }

    /// A barge-in cut the reply: the Echo dropped what it had not played.
    pub(crate) fn cut(&mut self, now: Instant) {
        if let Some(state) = &mut self.state {
            state.far.retain(|(start, _)| *start < now);
            state.playout = state.playout.map(|_| now);
            state.next = now;
            state.partial.clear();
        }
    }

    /// What was cancelled since this was last asked; none while off. The
    /// filter and the delay AEC3 found are kept for the run.
    pub(crate) fn rest(&mut self) -> Option<Cancelled> {
        let report = self.state.as_mut()?.aec.report();
        Some(Cancelled {
            delay_ms: u16::try_from(report.delay_ms.unwrap_or(0)).unwrap_or(u16::MAX),
            erle_db10: i16::try_from(report.erle_db10).unwrap_or(i16::MAX),
            converged: report.converged,
        })
    }
}

impl State {
    fn played(&mut self, frame: &[u8], now: Instant) -> Result<(), Error> {
        let start = self.playout.filter(|end| *end > now).unwrap_or(now);
        if self.playout != Some(start) {
            // The Echo had run dry: playout starts again from this frame.
            self.next = start;
            self.partial.clear();
        }
        self.playout = Some(start + WRITTEN);
        let (samples, _) = frame.as_chunks::<2>();
        let samples: Vec<i16> = samples.iter().map(|sample| i16::from_le_bytes(*sample)).collect();
        let mut offset = 0;
        while offset < samples.len() {
            let progress = self.resampler.process(&samples[offset..], &mut self.scratch)?;
            offset += progress.consumed;
            self.partial.extend_from_slice(&self.scratch[..progress.written]);
        }
        while self.partial.len() >= ECHO_FRAME {
            let mut far = [0; ECHO_FRAME];
            far.copy_from_slice(&self.partial[..ECHO_FRAME]);
            self.partial.drain(..ECHO_FRAME);
            if self.far.len() == BACKLOG {
                self.far.pop_front();
            }
            self.far.push_back((self.next, far));
            self.next += PERIOD;
        }
        Ok(())
    }

    /// The capture time of the frame that arrived at `now`. The microphone
    /// keeps its own time, 20 ms a frame, so that frames arriving together
    /// stay apart. No frame arrives before it was captured: a clock ahead of
    /// the latest possible capture is set back to it, which settles the
    /// clock on the frames that arrive soonest.
    fn clock(&mut self, now: Instant) -> Instant {
        let arrived = now.checked_sub(WRITTEN).unwrap_or(now);
        let at = self.microphone.filter(|at| *at <= arrived).unwrap_or(arrived);
        let behind = arrived.saturating_duration_since(at);
        self.late = if behind > LATE { self.late + 1 } else { 0 };
        if self.late > LOST || behind > BEHIND {
            self.late = 0;
            return arrived;
        }
        at
    }

    /// The far-end frame whose playout holds `at`, or silence.
    fn due(&mut self, at: Instant) -> EchoFrame {
        while self.far.front().is_some_and(|(start, _)| *start + PERIOD <= at) {
            self.far.pop_front();
        }
        match self.far.front() {
            Some((start, _)) if *start <= at => self.far.pop_front().map_or([0; ECHO_FRAME], |(_, far)| far),
            _ => [0; ECHO_FRAME],
        }
    }

    fn cleaned(&mut self, frame: &PcmFrame, now: Instant) -> Result<PcmFrame, Error> {
        if self.failed {
            return Err(Error::Audio);
        }
        let mut at = self.clock(now);
        if self.playout.is_none() {
            self.microphone = Some(at + WRITTEN);
            return Ok(*frame);
        }
        let mut clean = *frame;
        let (halves, _) = clean.as_chunks_mut::<ECHO_FRAME>();
        for near in halves {
            let far = self.due(at);
            self.aec.render(&far)?;
            self.aec.capture(near)?;
            at += PERIOD;
        }
        self.microphone = Some(at);
        Ok(clean)
    }
}

#[cfg(test)]
#[path = "reply_cancel_tests.rs"]
mod tests;
