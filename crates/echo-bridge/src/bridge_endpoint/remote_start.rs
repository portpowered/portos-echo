//! Command deadlines and one first text turn owned by a voice run.
use crate::Error;
use echo_contracts::endpoint::{AssistantState, Reply, StartSession, StartSessionReply};
use std::{
    future::Future,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::{oneshot, watch},
    time::{Duration, Instant},
};

pub(crate) struct Start {
    pub(crate) payload: StartSession,
    pub(crate) received: Instant,
    lease: watch::Receiver<()>,
    reply: oneshot::Sender<Reply>,
}

impl Start {
    pub(super) fn new(
        payload: StartSession,
        received: Instant,
        lease: watch::Receiver<()>,
    ) -> (Self, oneshot::Receiver<Reply>) {
        let (reply, outcome) = oneshot::channel();
        (
            Self {
                payload,
                received,
                lease,
                reply,
            },
            outcome,
        )
    }

    pub(crate) fn reply_deadline(&self) -> Instant {
        self.received
            + if self.payload.initial_text.is_some() {
                Duration::from_secs(1)
            } else {
                Duration::from_millis(2500)
            }
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.reply.is_closed() || self.lease.has_changed().is_err() || Instant::now() >= self.reply_deadline()
    }

    pub(crate) fn lease(&self) -> watch::Receiver<()> {
        self.lease.clone()
    }

    pub(crate) fn refuse(self, outcome: Reply) {
        let _ = self.reply.send(outcome);
    }

    pub(crate) fn admit(mut self, id: &str, followup: bool) -> Run {
        let deadline = self.reply_deadline();
        let label = safe_reason(&self.payload.reason);
        let text = self.payload.initial_text.take();
        let reply = if text.is_some() {
            Some(self.reply)
        } else {
            let _ = self.reply.send(Reply::StartSession(StartSessionReply::admitted(
                id,
                AssistantState::Listening,
            )));
            None
        };
        Run {
            initial: Mutex::new(text.zip(reply).map(|(text, reply)| TextTurn { text, reply })),
            id: id.into(),
            deadline,
            lease: self.lease,
            label,
            followup: AtomicBool::new(followup),
        }
    }
}

pub(crate) struct Run {
    initial: Mutex<Option<TextTurn>>,
    id: String,
    deadline: Instant,
    pub(crate) lease: watch::Receiver<()>,
    label: &'static str,
    followup: AtomicBool,
}

struct TextTurn {
    text: String,
    reply: oneshot::Sender<Reply>,
}

tokio::task_local! { static INITIAL: Run; }

impl Run {
    pub(crate) async fn scope<T>(self, work: impl Future<Output = T>) -> T {
        INITIAL.scope(self, work).await
    }
}

pub(crate) fn remote() -> bool {
    INITIAL.try_with(|_| ()).is_ok()
}

pub(crate) fn reason() -> Option<&'static str> {
    INITIAL.try_with(|run| run.label).ok()
}

pub(crate) fn take_followup() -> bool {
    INITIAL
        .try_with(|run| run.followup.swap(false, Ordering::Relaxed))
        .unwrap_or(false)
}

pub(crate) async fn open<R, W, P>(
    call: &mut crate::bridge_codex::MediaCall<'_, R, W, P>,
    options: &crate::bridge_codex::Options<'_>,
    tools: Vec<echo_codex::DynamicTool>,
    deadline: Instant,
) -> Result<(), Error>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
    P: echo_media::MediaPeer,
{
    call.start(options, tools, deadline).await?;
    append(call).await
}

async fn append<R, W, P>(call: &mut crate::bridge_codex::MediaCall<'_, R, W, P>) -> Result<(), Error>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
    P: echo_media::MediaPeer,
{
    let initial = INITIAL
        .try_with(|run| {
            let mut turn = run.initial.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            turn.take().map(|turn| (turn, run.id.clone(), run.deadline))
        })
        .ok()
        .flatten();
    let Some((turn, id, deadline)) = initial else {
        return Ok(());
    };
    if turn.reply.is_closed() || Instant::now() >= deadline {
        return Err(Error::Deadline);
    }
    call.append_endpoint_text(&turn.text, deadline).await?;
    turn.reply
        .send(Reply::StartSession(StartSessionReply::admitted(
            &id,
            AssistantState::Thinking,
        )))
        .map_err(|_| Error::Control)?;
    if let Some(voice) = super::voice::Voice::current() {
        voice.remote_admitted(&id);
    }
    Ok(())
}

fn safe_reason(reason: &str) -> &'static str {
    if reason.trim().is_empty() {
        "unspecified"
    } else {
        "requested"
    }
}
