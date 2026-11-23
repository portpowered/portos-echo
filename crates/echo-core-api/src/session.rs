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
    Wake {
        session: SessionHandle,
        opened: Instant,
        listen: Option<Duration>,
        max: Duration,
        response_due: Instant,
        reuse_call: bool,
    },
    CallOpened {
        session: SessionHandle,
    },
    CallCloseDue,
    CaptureEnded {
        session: SessionHandle,
    },
    ReplyStarted {
        session: SessionHandle,
    },
    ReplyEnded {
        session: SessionHandle,
    },
    ReplyPlayed {
        session: SessionHandle,
        output_stopped: bool,
    },
    ReplySettled {
        session: SessionHandle,
    },
    Deadline {
        session: SessionHandle,
        timer: SessionTimer,
    },
    ExplicitEnd {
        session: SessionHandle,
    },
    RunReleased {
        session: SessionHandle,
    },
    Tool(SessionToolEvent),
    Turn(SessionTurnEvent),
    /// Additional playback observations available at the bridge's media fence.
    TurnObserved {
        turn: SessionTurnEvent,
        context: SessionTurnContext,
    },
    Run {
        session: SessionHandle,
        change: SessionChange,
    },
    Configure(SessionKnobs),
}

impl SessionEvent {
    #[must_use]
    pub fn session(&self) -> Option<SessionHandle> {
        match *self {
            Self::Wake { session, .. }
            | Self::CallOpened { session }
            | Self::CaptureEnded { session }
            | Self::ReplyStarted { session }
            | Self::ReplyEnded { session }
            | Self::ReplyPlayed { session, .. }
            | Self::ReplySettled { session }
            | Self::Deadline { session, .. }
            | Self::ExplicitEnd { session }
            | Self::RunReleased { session }
            | Self::Run { session, .. }
            | Self::Tool(SessionToolEvent::Started { session, .. } | SessionToolEvent::Ended { session, .. }) => {
                Some(session)
            }
            Self::Turn(ref turn) | Self::TurnObserved { ref turn, .. } => Some(turn.session()),
            Self::CallCloseDue | Self::Configure(_) => None,
        }
    }
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
    SpeechStarted {
        session: SessionHandle,
    },
    SpeechStopped {
        session: SessionHandle,
    },
    Transcript {
        session: SessionHandle,
        text: String,
        whole: bool,
    },
    AssistantAudio {
        session: SessionHandle,
        audible: bool,
    },
    ReplyFrameHanded {
        session: SessionHandle,
        voiced: bool,
    },
    DispatchStarted {
        session: SessionHandle,
    },
    DispatchCompleted {
        session: SessionHandle,
        answered: bool,
    },
    DispatchWhileListening {
        session: SessionHandle,
    },
    EndDispatch {
        session: SessionHandle,
        farewell: bool,
    },
    BargeDue {
        session: SessionHandle,
        finished: bool,
    },
    ReplyMeasured {
        session: SessionHandle,
        cut: bool,
    },
    RunEnded {
        session: SessionHandle,
    },
}

impl SessionTurnEvent {
    fn session(&self) -> SessionHandle {
        match *self {
            Self::SpeechStarted { session }
            | Self::SpeechStopped { session }
            | Self::Transcript { session, .. }
            | Self::AssistantAudio { session, .. }
            | Self::ReplyFrameHanded { session, .. }
            | Self::DispatchStarted { session }
            | Self::DispatchCompleted { session, .. }
            | Self::DispatchWhileListening { session }
            | Self::EndDispatch { session, .. }
            | Self::BargeDue { session, .. }
            | Self::ReplyMeasured { session, .. }
            | Self::RunEnded { session } => session,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct SessionTurnContext {
    pub sounded: bool,
    pub voiced: bool,
    pub listening: bool,
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
    Resume {
        preserve_dispatch: bool,
    },
    Listen(Duration),
    ConfigureTools {
        listen: Option<Duration>,
        max: Duration,
    },
    RenewIdle,
    Cut,
    CutSettled,
    CutPlayed,
    Playback(SessionPlayback),
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
    pub idle_deadline: Option<Instant>,
    pub next_deadline: Option<(Instant, SessionTimer)>,
}

#[derive(PartialEq, Eq)]
pub enum SessionCommand {
    OpenCall {
        session: SessionHandle,
    },
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
        trigger: Option<BargeTrigger>,
    },
    Tool(SessionToolDecision),
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
        session: SessionHandle,
        tool: ToolCallHandle,
        action: SessionToolAction,
    },
    Ended {
        session: SessionHandle,
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
    pub effects: [Option<SessionCommand>; 2],
    pub cut_text: String,
    pub outcome: Option<Result<SessionOutcome, SessionError>>,
}

impl SessionCommands {
    pub fn take_effect(&mut self) -> Option<SessionCommand> {
        let next = self.effects[1].take();
        std::mem::replace(&mut self.effects[0], next)
    }
}
