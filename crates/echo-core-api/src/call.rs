use std::num::NonZeroU64;

pub type CallPcmFrame = [i16; 320];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallHandle(pub NonZeroU64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CallPhase {
    #[default]
    Fresh,
    Opening,
    Connected,
    Closing,
    Closed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CallReceive {
    #[default]
    Current,
    Retired,
    Resetting,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CallSetupStage {
    #[default]
    New,
    SignalingStarted,
    AnswerReceived,
    AnswerApplied,
    MediaReady,
}

impl CallSetupStage {
    #[must_use]
    pub const fn diagnostic(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::SignalingStarted => "signaling_started",
            Self::AnswerReceived => "answer_received",
            Self::AnswerApplied => "answer_applied",
            Self::MediaReady => "media_ready",
        }
    }
}

#[derive(Clone, Copy)]
pub enum CallEvent<'a> {
    PeerAttached,
    OpenRequested {
        call: CallHandle,
    },
    SetupStage {
        call: CallHandle,
        stage: CallSetupStage,
    },
    PeerConnected {
        call: CallHandle,
    },
    PeerFailed {
        call: CallHandle,
    },
    CloseRequested {
        call: CallHandle,
    },
    PeerClosed {
        call: CallHandle,
    },
    RetireReceive {
        call: CallHandle,
    },
    BumpReceiveGeneration {
        call: CallHandle,
    },
    ReceiveReset {
        call: CallHandle,
        generation: u64,
    },
    RemoteAudioFrame {
        call: CallHandle,
        generation: u64,
        frame: &'a CallPcmFrame,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallCommand {
    OpenPeer(CallHandle),
    ClosePeer(CallHandle),
    ResetReceive { call: CallHandle, generation: u64 },
    DeliverRemoteAudio { call: CallHandle, generation: u64 },
}

#[derive(Default)]
pub struct CallCommands {
    action: Option<CallCommand>,
    frame: Option<CallPcmFrame>,
}

impl CallCommands {
    pub fn emit(&mut self, action: CallCommand, frame: Option<&CallPcmFrame>) {
        self.action = Some(action);
        self.frame = frame.copied();
    }

    #[must_use]
    pub fn into_parts(self) -> (Option<CallCommand>, Option<CallPcmFrame>) {
        (self.action, self.frame)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallView {
    pub handle: Option<CallHandle>,
    pub phase: CallPhase,
    pub setup_stage: CallSetupStage,
    pub receive: CallReceive,
    pub receive_generation: u64,
}
