use std::time::Duration;

use crate::Command;

pub type ReplyFrame = [u8; 960];

pub enum PacerEvent {
    ConfigureLips { delay: Option<Duration> },
    RunOpened { lead_ms: u16, adaptive: bool },
    ReplyOpened,
    PeerFrameArrived { audible: bool },
    PcmPushed(Box<ReplyFrame>),
    MicrophoneFrame(Box<[i16; 320]>),
    InputFinished,
    Offer { allow_fade: bool },
    Tick,
    FrameHanded { screen_wanted: Option<bool> },
    LipDelivered { taken: bool },
    MouthSettled,
    Played,
    Cut,
    ReplySettled,
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
    frame: Option<ReplyFrame>,
    lips: Option<PacerLipBatch>,
    mouth_changed: Option<bool>,
    finished: bool,
}

impl PacerCommands {
    pub fn write_frame(&mut self, frame: &ReplyFrame) {
        self.frame = Some(*frame);
    }

    pub fn publish_lips(&mut self, batch: Option<PacerLipBatch>) {
        self.lips = batch;
    }

    pub fn change_mouth(&mut self, open: Option<bool>) {
        self.mouth_changed = open;
    }

    pub fn finish_playout(&mut self) {
        self.finished = true;
    }

    pub fn take_frame(&mut self) -> Option<ReplyFrame> {
        self.frame.take()
    }

    pub fn take_lips(&mut self) -> Option<PacerLipBatch> {
        self.lips.take()
    }

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
