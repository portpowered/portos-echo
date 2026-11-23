use echo_audio::{FrameQueue, Pacer};
use std::time::Duration;
use tokio::time::Instant;

#[cfg(test)]
mod playout_tests;
pub(crate) mod reply_lips;
#[cfg(test)]
#[path = "bridge_pacing/reply_lips_tests.rs"]
mod reply_lips_tests;

pub use echo_core_api::BargeTrigger;
pub use echo_sub_pacer::{Heard, LipStats};
pub(crate) use echo_sub_pacer::{PERIOD, Playout, QUIET};
#[cfg(test)]
pub(crate) use echo_sub_session::{BARGE_QUIET, BARGE_WINDOW};

/// Initial playback credit followed by the existing no-catch-up 20 ms pacer.
pub(crate) struct LeadPacer {
    remaining: u16,
    started: Option<Instant>,
    pacer: Pacer,
}

impl LeadPacer {
    pub(crate) fn new(lead_ms: u16) -> Self {
        Self {
            remaining: lead_ms / 20,
            started: None,
            pacer: Pacer::new(),
        }
    }

    pub(crate) async fn next_frame<T>(&mut self, queue: &mut FrameQueue<T>) -> Option<T> {
        if queue.is_empty() {
            return None;
        }
        let started = *self.started.get_or_insert_with(|| {
            self.pacer = Pacer::new();
            Instant::now()
        });
        // Underflow never renews credit, and a late producer never catches up.
        if Instant::now() >= started + Duration::from_millis(20) {
            self.remaining = 0;
        }
        if self.remaining > 0 {
            self.remaining -= 1;
            queue.pop()
        } else {
            self.pacer.next_frame(queue).await
        }
    }
}

/// One reply's audio facts for `reply.played`. Depth is the Echo's modelled
/// playout queue: frames written play back to back from the first write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplyQuality {
    /// Longest wait between two accepted frames from the call.
    pub arrival_gap_max_ms: u32,
    /// Accepted frames that came more than two periods after the previous one.
    pub late_arrivals: u32,
    /// Writes that found the modelled queue empty after playback had started.
    pub underruns: u32,
    pub underrun_max_ms: u32,
    /// Queue depth found by each write after the first.
    pub depth_min_ms: u32,
    pub depth_max_ms: u32,
    pub discarded_after_reply: u32,
    pub discarded_idle: u32,
    pub discarded_cut: u32,
    /// Discarded frames, for any reason, that held more than silence.
    pub discarded_audible: u32,
    /// RTP packets missing from the receive sequence during this reply.
    pub rtp_lost: u64,
    /// The playout cushion in force as each frame was accepted: least, most
    /// and last. 0 under the sim's stepped clock, which holds nothing.
    pub target_min_ms: u32,
    pub target_max_ms: u32,
    pub target_final_ms: u32,
    /// Starts after the Echo had played out everything sent, mid-reply.
    pub reprimes: u32,
    /// Fade-out audio written just before the Echo ran dry.
    pub concealment_ms: u32,
    /// Frames ramped up from silence after a fade-out.
    pub fade_ins: u32,
    /// Silent frames left out to bring the delay back to the target.
    pub drained_frames: u32,
    /// Barge-ins that cut this reply, and what armed the last of them.
    pub cuts: u32,
    pub cut_trigger: Option<BargeTrigger>,
    /// Times a sign of user speech held the reply back from the device,
    /// and for how long in all: to the cut, or to the release.
    pub holds: u32,
    pub hold_ms: u32,
    /// What the microphone heard under this reply.
    pub mic: Heard,
    /// What the screen was sent to move the mouth with this reply.
    pub lips: LipStats,
    /// The `timing` sample; none when no endpoint preceded the reply's audio.
    pub first_audio: Option<FirstAudio>,
}

/// The barge-in that cut a reply, from the Live session's sign of user speech.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cut {
    /// The session's transcript of the user under it, bounded.
    pub text: String,
    /// To the last reply frame the Echo played, by the modelled playout queue.
    pub silent_ms: u32,
    /// To the cut: the model's yield.
    pub yield_ms: u32,
}

/// How the bridge learned the user's turn had ended. The pinned V3 session
/// forwards no speech-stopped event, so these are the two signs it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// A user-role transcript event from the Live session.
    UserTranscript,
    /// The Live session started a delegated turn for the request.
    DelegatedTurn,
}

impl Endpoint {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::UserTranscript => "user_transcript",
            Self::DelegatedTurn => "delegated_turn",
        }
    }
}

/// From the endpoint to the first reply frame handed to the Echo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstAudio {
    pub after_endpoint_ms: u32,
    pub endpoint: Endpoint,
}

/// Why accepted call audio never reached the reply queue.
#[derive(Clone, Copy)]
pub(crate) enum Discard {
    AfterReply,
    Idle,
    Cut,
}

#[derive(Default)]
pub(crate) struct ReplyMeter {
    quality: ReplyQuality,
    arrived: Option<Instant>,
    playout: Option<Instant>,
    measured: bool,
    lost: u64,
    endpoint: Option<(Instant, Endpoint)>,
    /// A cut flushed the device: the next write starts its clock again.
    restarted: bool,
    /// When the pending hold began, and how long the Echo then still played.
    holding: Option<(Instant, u32)>,
    targeted: bool,
}

fn millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

impl ReplyMeter {
    /// One reply frame was accepted at `now`.
    pub(crate) fn arrived(&mut self, now: Instant) {
        if let Some(last) = self.arrived.replace(now) {
            let gap = now.saturating_duration_since(last);
            self.quality.arrival_gap_max_ms = self.quality.arrival_gap_max_ms.max(millis(gap));
            if gap > PERIOD * 2 {
                self.quality.late_arrivals += 1;
            }
        }
    }

    /// The cushion the pacer put in force for the frame just accepted.
    pub(crate) fn targeted(&mut self, target: Duration) {
        let target = millis(target);
        let quality = &mut self.quality;
        quality.target_min_ms = if self.targeted {
            quality.target_min_ms.min(target)
        } else {
            target
        };
        quality.target_max_ms = quality.target_max_ms.max(target);
        quality.target_final_ms = target;
        self.targeted = true;
    }

    /// What a pacer did since it was last asked.
    pub(crate) fn paced(&mut self, playout: Playout) {
        self.quality.reprimes += playout.reprimes;
        self.quality.concealment_ms += playout.concealed * millis(PERIOD);
        self.quality.fade_ins += playout.fade_ins;
        self.quality.drained_frames += playout.drained;
    }

    /// The Live session showed at `now` that the user's turn is over. The
    /// first sign before this reply's audio is its endpoint. One that comes
    /// once the reply is playing belongs to no reply and is not kept.
    pub(crate) fn endpoint(&mut self, now: Instant, endpoint: Endpoint) {
        if self.playout.is_none() && self.endpoint.is_none() {
            self.endpoint = Some((now, endpoint));
        }
    }

    /// One 20 ms frame was handed to the Echo at `now`.
    pub(crate) fn wrote(&mut self, now: Instant) {
        let Some(end) = self.playout else {
            self.playout = Some(now + PERIOD);
            self.quality.first_audio = self.endpoint.take().map(|(at, endpoint)| FirstAudio {
                after_endpoint_ms: millis(now.saturating_duration_since(at)),
                endpoint,
            });
            return;
        };
        if std::mem::take(&mut self.restarted) {
            self.playout = Some(now + PERIOD);
            return;
        }
        let depth = millis(end.saturating_duration_since(now));
        if depth == 0 {
            self.quality.underruns += 1;
            self.quality.underrun_max_ms = self
                .quality
                .underrun_max_ms
                .max(millis(now.saturating_duration_since(end)));
        }
        self.quality.depth_min_ms = if self.measured {
            self.quality.depth_min_ms.min(depth)
        } else {
            depth
        };
        self.quality.depth_max_ms = self.quality.depth_max_ms.max(depth);
        self.measured = true;
        self.playout = Some(end.max(now) + PERIOD);
    }

    pub(crate) fn discarded(&mut self, reason: Discard, audible: bool) {
        let count = match reason {
            Discard::AfterReply => &mut self.quality.discarded_after_reply,
            Discard::Idle => &mut self.quality.discarded_idle,
            Discard::Cut => &mut self.quality.discarded_cut,
        };
        *count += 1;
        self.quality.discarded_audible += u32::from(audible);
    }

    /// A barge-in cut the reply: the device was flushed and plays again from
    /// the replacement's first frame. The retired stretch between the two is
    /// neither an arrival gap nor an underrun; `cuts` and the discards say it.
    /// Returns how long the Echo still played after the sign that held the
    /// reply, and how long after that sign the cut came.
    pub(crate) fn cut(&mut self, trigger: BargeTrigger) -> (u32, u32) {
        self.quality.cuts += 1;
        self.quality.cut_trigger = Some(trigger);
        self.arrived = None;
        let now = Instant::now();
        let (since, silent) = self.holding.unwrap_or((now, 0));
        self.released(now);
        (silent, millis(now.saturating_duration_since(since)))
    }

    /// A sign of user speech holds the reply back from the device at `now`.
    pub(crate) fn held(&mut self, now: Instant) {
        self.quality.holds += 1;
        let playing = self
            .playout
            .map_or(Duration::ZERO, |end| end.saturating_duration_since(now));
        self.holding = Some((now, millis(playing)));
    }

    /// The hold ended at `now`. If the model talked through it, the held
    /// audio plays from here: the device ran dry on purpose, which is not an
    /// underrun.
    pub(crate) fn released(&mut self, now: Instant) {
        if let Some((since, _)) = self.holding.take() {
            self.quality.hold_ms += millis(now.saturating_duration_since(since));
        }
        self.restarted = self.playout.is_some();
    }

    /// Close this reply's window. `lost` is the call's cumulative RTP loss.
    pub(crate) fn settle(&mut self, lost: u64) -> ReplyQuality {
        let mut quality = std::mem::take(&mut self.quality);
        quality.rtp_lost = lost.saturating_sub(self.lost);
        *self = Self {
            lost,
            ..Self::default()
        };
        quality
    }
}

#[cfg(test)]
mod bridge_pacing_tests {
    use super::*;
    use std::{future::Future, num::NonZeroUsize, task::Poll};

    fn queue() -> FrameQueue<u16> {
        let mut queue = FrameQueue::new(NonZeroUsize::new(30).unwrap());
        for frame in 0..30 {
            queue.push(frame);
        }
        queue
    }

    #[tokio::test(start_paused = true)]
    async fn reply_meter_measures_first_audio_from_the_first_endpoint_before_it() {
        let mut meter = ReplyMeter::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        // No endpoint before the audio: the reply has no sample.
        meter.wrote(at(5));
        meter.endpoint(at(10), Endpoint::UserTranscript);
        meter.wrote(at(25));
        assert_eq!(meter.settle(0).first_audio, None);
        // That late endpoint was not kept, and the first of two signs counts.
        meter.wrote(at(40));
        assert_eq!(meter.settle(0).first_audio, None);
        meter.endpoint(at(100), Endpoint::DelegatedTurn);
        meter.endpoint(at(400), Endpoint::UserTranscript);
        meter.wrote(at(1337));
        meter.wrote(at(1357));
        let sample = FirstAudio {
            after_endpoint_ms: 1237,
            endpoint: Endpoint::DelegatedTurn,
        };
        assert_eq!(meter.settle(0).first_audio, Some(sample));
        assert_eq!(
            (Endpoint::UserTranscript.name(), Endpoint::DelegatedTurn.name()),
            ("user_transcript", "delegated_turn")
        );
        // A frame written at the endpoint's own instant is a zero sample.
        meter.endpoint(at(2000), Endpoint::UserTranscript);
        meter.wrote(at(2000));
        let first = meter.settle(0).first_audio.map(|first| first.after_endpoint_ms);
        assert_eq!(first, Some(0));
    }

    #[test]
    fn reply_meter_counts_discards_by_reason_and_settles_per_reply() {
        let mut meter = ReplyMeter::default();
        meter.discarded(Discard::AfterReply, true);
        meter.discarded(Discard::Idle, false);
        meter.discarded(Discard::Cut, true);
        meter.discarded(Discard::Cut, false);
        let first = meter.settle(5);
        assert_eq!(
            (first.discarded_after_reply, first.discarded_idle, first.discarded_cut),
            (1, 1, 2)
        );
        assert_eq!((first.discarded_audible, first.rtp_lost), (2, 5));
        let second = meter.settle(7);
        assert_eq!(
            second,
            ReplyQuality {
                rtp_lost: 2,
                ..ReplyQuality::default()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_pacing_ten_initial_frames_then_twenty_ms_without_catchup() {
        let mut queue = queue();
        let mut pacer = LeadPacer::new(200);
        let start = Instant::now();
        for frame in 0..10 {
            assert_eq!(pacer.next_frame(&mut queue).await, Some(frame));
        }
        assert_eq!(Instant::now(), start);
        assert_eq!(pacer.next_frame(&mut queue).await, Some(10));
        assert_eq!(Instant::now() - start, Duration::from_millis(20));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(pacer.next_frame(&mut queue).await, Some(11));
        let late = Instant::now();
        assert_eq!(pacer.next_frame(&mut queue).await, Some(12));
        assert_eq!(Instant::now() - late, Duration::from_millis(20));
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_pacing_underflow_and_cancel_never_add_credit_or_consume_frames() {
        let mut queue = queue();
        let mut pacer = LeadPacer::new(20);
        assert_eq!(pacer.next_frame(&mut queue).await, Some(0));
        let mut next = Box::pin(pacer.next_frame(&mut queue));
        std::future::poll_fn(|cx| {
            assert!(next.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(next);
        assert_eq!(queue.len(), 29);
        queue.clear();
        assert_eq!(pacer.next_frame(&mut queue).await, None);
        tokio::time::advance(Duration::from_secs(1)).await;
        queue.push(40);
        queue.push(41);
        let now = Instant::now();
        assert_eq!(pacer.next_frame(&mut queue).await, Some(40));
        assert_eq!(pacer.next_frame(&mut queue).await, Some(41));
        assert_eq!(Instant::now() - now, Duration::from_millis(20));
    }
}
