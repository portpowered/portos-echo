use std::time::Duration;

use crate::Command;

pub const REPLY_CAPACITY: usize = 128;
pub const REPLY_PERIOD: Duration = Duration::from_millis(20);
/// Samples at or below this level count as WebRTC silence.
pub const REPLY_QUIET: u16 = 64;
pub type ReplyFrame = [u8; 960];

pub enum PacerEvent {
    ConfigureLips { delay: Option<Duration> },
    RunOpened { lead_ms: u16, adaptive: bool },
    ExpectJitter,
    ReplyOpened,
    PeerFrameArrived { audible: bool },
    PeerFrameDiscarded,
    PcmPushed(Box<ReplyFrame>),
    MicrophoneFrame(Box<[i16; 320]>),
    InputFinished,
    Offer { allow_fade: bool },
    Tick,
    FrameHanded { screen_wanted: Option<bool> },
    LipDelivered { taken: bool },
    MouthSettled,
    MouthTick,
    Played,
    Cut,
    ReplySettled,
    TakeLipStats,
    RunClosed,
}

pub enum PacerCommand {
    WriteFrame,
    LipBatch,
    MouthChanged(bool),
    PlayoutFinished,
}

#[derive(Default)]
pub struct PacerCommands {
    pub frame: Option<ReplyFrame>,
    pub lips: Option<PacerLipBatch>,
    pub mouth_changed: Option<bool>,
    pub finished: bool,
    /// Measurements emitted at admission, reply reset, or settlement.
    pub target: Option<Duration>,
    pub playout: Playout,
    pub heard: Heard,
    pub lip_stats: LipStats,
}

impl PacerCommands {
    /// The fixed-size payloads are drained separately from their notifications.
    #[must_use]
    pub fn notification(&self) -> Option<Command> {
        let kind = if self.frame.is_some() {
            PacerCommand::WriteFrame
        } else if self.lips.is_some() {
            PacerCommand::LipBatch
        } else if let Some(open) = self.mouth_changed {
            PacerCommand::MouthChanged(open)
        } else if self.finished {
            PacerCommand::PlayoutFinished
        } else {
            return None;
        };
        Some(Command::Pacer(kind))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PacerView {
    pub depth: usize,
    pub playing: bool,
    pub last_frame_at: Option<Duration>,
}

/// The voice owner's readiness predicates and absolute World deadlines.
#[derive(Clone, Copy)]
pub struct PacerStatus {
    pub view: PacerView,
    pub input: PacerInput,
    pub output: PacerOutput,
    pub spoken: bool,
    pub sounded: bool,
    pub dropped: u64,
    pub deadline: Option<Duration>,
    pub mouth_due: Option<Duration>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PacerInput {
    Closed,
    Open,
    Finished,
    Cut,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PacerOutput {
    Idle,
    Staged,
    Stalled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacerVowel {
    None,
    A,
    I,
    U,
    E,
    O,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PacerLipSample {
    pub at_ms: u64,
    pub open: u8,
    pub vowel: PacerVowel,
}

pub struct PacerLipBatch {
    pub sent_ms: u64,
    pub samples: [Option<PacerLipSample>; 5],
}

/// What the pacer did to keep one reply's audio whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Playout {
    pub reprimes: u32,
    pub concealed: u32,
    pub fade_ins: u32,
    pub drained: u32,
}

/// What the microphone heard under one reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Heard {
    pub onsets: u32,
    pub echo_max: u32,
    pub level_max: u32,
    pub coupling: u32,
    pub echo_mean: u32,
    pub noise: u32,
}

/// What the screen was sent to move the mouth with one reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LipStats {
    pub batches: u32,
    pub samples: u32,
    pub dropped: u32,
    pub publish_max_ms: u32,
    pub lead_min_ms: u32,
    pub late: u32,
}
