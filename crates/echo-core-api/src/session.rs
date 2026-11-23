//! In-process session observations and effects.
use std::{
    num::NonZeroU64,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionHandle {
    pub index: usize,
    pub generation: NonZeroU64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolCallHandle {
    pub index: usize,
    pub generation: NonZeroU64,
}

#[derive(Clone, Copy)]
pub enum SessionEvent {
    CallAdmitted,
    CallCloseDue,
    Wake {
        session: SessionHandle,
        epoch: Instant,
        opened: Instant,
        max: Option<Duration>,
    },
    Run {
        session: SessionHandle,
        change: SessionChange,
    },
    Tool {
        session: SessionHandle,
        change: SessionToolEvent,
    },
    RunReleased {
        session: SessionHandle,
    },
}

#[derive(Clone, Copy)]
pub enum SessionChange {
    AdmitResponse(Instant),
    ResponseDue(Instant),
    CaptureStarted,
    CaptureEnded,
    ReplyStarted,
    ReplyPlayed { output_stopped: bool },
    ReplySettled,
    Resume,
    Listen(Duration),
    ConfigureTools { listen: Option<Duration>, max: Duration },
    RenewIdle,
    RequestEnd,
    Cut,
    CutSettled,
    CutPlayed,
    Unanswered,
    Deadline { timer: SessionTimer, due: Instant },
    BeginClose { mouth_drained: bool },
    FinishClose { mouth_drained: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTimer {
    Listen,
    Response,
    RunCeiling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowUp {
    First,
    Replied,
    Unanswered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionView {
    pub session: Option<SessionHandle>,
    pub closing: bool,
    pub speaking: bool,
    pub microphone_finished: bool,
    pub accepts_reply_audio: bool,
    pub idle_deadline: Option<Instant>,
    pub next_deadline: Option<(Instant, SessionTimer)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    OpenCall,
    CloseCall,
    StartCapture { session: SessionHandle },
    StopCapture { session: SessionHandle },
    EndSession { session: SessionHandle },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    Input,
    Control,
    Deadline,
    Playback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionOutcome {
    Applied,
    Played(bool),
    DeadlineReached(bool),
    Tool(SessionToolDecision),
}

#[derive(Clone, Copy)]
pub enum SessionToolAction {
    Extend { seconds: u64 },
    End,
    Invalid,
}

#[derive(Clone, Copy)]
pub enum SessionToolEvent {
    Started {
        tool: ToolCallHandle,
        action: SessionToolAction,
    },
    Ended {
        tool: ToolCallHandle,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionToolDecision {
    Extended {
        tool: ToolCallHandle,
        deadline_ms: u64,
        capped: bool,
    },
    Ending {
        tool: ToolCallHandle,
        duplicate: bool,
    },
    Refused {
        tool: ToolCallHandle,
        reason: SessionToolRefusal,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionToolRefusal {
    InvalidArguments,
    Inactive,
    Closing,
}

#[derive(Default)]
pub struct SessionCommands {
    pub effect: Option<SessionCommand>,
    pub outcome: Option<Result<SessionOutcome, SessionError>>,
}
