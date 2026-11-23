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
    Turn {
        session: SessionHandle,
        change: SessionTurnEvent,
    },
    Configure(SessionKnobs),
    Tool {
        session: SessionHandle,
        change: SessionToolEvent,
    },
    RunReleased {
        session: SessionHandle,
    },
}

pub const SPEECH_START_EXPIRY: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug)]
pub struct SessionKnobs {
    pub expiry: Duration,
    pub local_cut: bool,
}

impl Default for SessionKnobs {
    fn default() -> Self {
        Self {
            expiry: SPEECH_START_EXPIRY,
            local_cut: true,
        }
    }
}

pub enum SessionTurnEvent {
    SpeechStarted,
    SpeechStopped {
        voiced: bool,
    },
    Transcript {
        text: String,
        whole: bool,
        sounded: bool,
        voiced: bool,
    },
    AssistantStarted,
    AssistantAudio {
        audible: bool,
    },
    ReplyFrameHanded {
        voiced: bool,
    },
    ReplyDone,
    DispatchStarted,
    DispatchCompleted {
        answered: bool,
        listening: bool,
    },
    DispatchWhileListening,
    EndDispatch {
        farewell: bool,
    },
    BargeDue {
        finished: bool,
    },
    ReplyMeasured {
        cut: bool,
    },
    RunEnded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BargeTrigger {
    UserTranscript,
    SpeechStarted,
}

impl BargeTrigger {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::UserTranscript => "user_transcript",
            Self::SpeechStarted => "speech_started",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMark {
    Latched {
        held: Duration,
        reason: &'static str,
        transcribed: bool,
    },
    Capped(&'static str),
    Lapsed(&'static str),
    BargeCut {
        turn: u32,
        trigger: BargeTrigger,
    },
    BargePassed {
        turn: u32,
        trigger: BargeTrigger,
        count: u32,
    },
    BargeRearmed {
        turn: u32,
        reason: &'static str,
    },
    BargeLeft {
        turn: u32,
        trigger: BargeTrigger,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionWait {
    Playback,
    Response,
    Ceiling,
    Listen,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionPlayback {
    pub played_due: Option<Instant>,
    pub finished: bool,
    pub voiced: bool,
}

#[derive(Clone, Copy)]
pub enum SessionChange {
    AdmitResponse(Instant),
    ResponseDue(Instant),
    CaptureStarted,
    CaptureEnded,
    ReplyStarted,
    ReplyPlayed {
        output_stopped: bool,
    },
    ReplySettled,
    Resume {
        preserve_dispatch: bool,
    },
    Listen(Duration),
    ConfigureTools {
        listen: Option<Duration>,
        max: Duration,
    },
    RenewIdle,
    RequestEnd,
    Cut,
    CutSettled,
    CutPlayed,
    Playback(SessionPlayback),
    Deadline {
        timer: SessionTimer,
        due: Instant,
    },
    WaitExpired {
        wait: SessionWait,
        due: Option<Instant>,
        voiced: bool,
        listening: bool,
    },
    BeginClose {
        mouth_drained: bool,
    },
    FinishClose {
        mouth_drained: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTimer {
    Listen,
    Response,
    RunCeiling,
    SpeechLatch,
    Barge,
    Playback,
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
    StartCapture {
        session: SessionHandle,
    },
    StopCapture {
        session: SessionHandle,
    },
    EndSession {
        session: SessionHandle,
    },
    CutReply {
        session: SessionHandle,
        trigger: BargeTrigger,
    },
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
    Held,
    Released,
    FinishReply,
    WaitExpired,
    Unanswered,
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
    pub cut_text: String,
}
