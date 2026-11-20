//! Caller-driven tool work shares the media owner's single process reader.
use super::{Event, MediaCall, MediaEvent, RetainedCall};
use crate::{Error, bridge_voice::VoicePort};
use echo_agent_api::ToolHost;
use echo_codex::codex_protocol::{RequestId, ToolContent, ToolResult};
use echo_config::AgentConfig;
use echo_delegation::codex_dispatch::{Catalog, DailyBudget, Dispatcher, Outcome};
use echo_media::{MediaPeer, PcmFrame};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, oneshot},
    time::Instant,
};

type Work<H> = Pin<Box<dyn Future<Output = (Dispatcher<H>, Option<Outcome>)>>>;

/// Validated bridge-owned session control; raw arguments never leave the host.
/// `Invalid` carries rejected arguments for the named builtin (`end` for
/// `end_session`), so the controller still writes the receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    Extend(u64),
    End,
    Invalid { end: bool },
}

/// One admitted session tool invocation. Only the run controller answers it;
/// a dropped request fails the tool instead of reporting success.
pub struct SessionRequest {
    pub command: SessionCommand,
    pub key: String,
    pub run: Option<Arc<()>>,
    pub reply: oneshot::Sender<echo_agent_api::ToolResult>,
}

/// Process-owned accounting shared across call and supervisor generations.
#[derive(Clone)]
pub struct ToolBudget {
    counter: Arc<Mutex<DailyBudget>>,
    utc_day: Arc<dyn Fn() -> Result<u64, Error> + Send + Sync>,
}

impl ToolBudget {
    #[must_use]
    pub fn new(cap: u64, utc_day: impl Fn() -> Result<u64, Error> + Send + Sync + 'static) -> Self {
        Self {
            counter: Arc::new(Mutex::new(DailyBudget::new(cap))),
            utc_day: Arc::new(utc_day),
        }
    }

    /// Check before opening a Live call, without spending a turn allowance.
    /// # Errors
    /// Clock failure, poisoned ownership or exhausted UTC-day allowance.
    pub fn check_call(&self) -> Result<(), Error> {
        let day = (self.utc_day)()?;
        self.counter
            .lock()
            .map_err(|_| Error::Unavailable)?
            .check(day)
            .map_err(|_| Error::Unavailable)
    }

    /// Admit before a bridge-initiated turn, or interrupt an observed automatic turn.
    /// # Errors
    /// Clock failure, poisoned ownership or exhausted UTC-day allowance.
    pub fn admit_turn(&self) -> Result<(), Error> {
        let day = (self.utc_day)()?;
        self.counter
            .lock()
            .map_err(|_| Error::Unavailable)?
            .admit(day)
            .map_err(|_| Error::Unavailable)
    }
}

/// One connected thread, one dispatcher and one bounded pending attempt.
/// The future is retained across cancelled `next` polls; no task is detached.
/// Turn completion cancels unfinished work before admitting the next turn.
/// Confirmation and MCP authorization remain exclusively at the supplied host.
/// The caller checks daily admission before opening the call and advertises
/// this catalog. Automatic model routing before notification cannot be undone.
pub struct ToolSession<'p, 'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer, H> {
    port: &'p mut MediaCall<'a, R, W, P>,
    dispatcher: Option<Dispatcher<H>>,
    work: Option<Work<H>>,
    request: Option<RequestId>,
    cancel: Option<oneshot::Sender<()>>,
    active: Option<(String, String)>,
    deferred: VecDeque<Event>,
    budget: ToolBudget,
    session: Option<mpsc::Receiver<SessionRequest>>,
    run: Option<tokio::sync::watch::Sender<Option<Arc<()>>>>,
    current_run: Option<Arc<()>>,
    session_answer_pending: bool,
}

impl<'p, 'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer, H: ToolHost + 'static>
    ToolSession<'p, 'a, R, W, P, H>
{
    /// Bind only after call negotiation; an old dispatcher is never reused.
    /// # Errors
    /// Missing thread or invalid dispatcher configuration.
    pub fn new(
        port: &'p mut MediaCall<'a, R, W, P>,
        host: H,
        catalog: Catalog,
        config: AgentConfig,
        budget: ToolBudget,
    ) -> Result<Self, Error> {
        let thread = port.thread_id().ok_or(Error::Input)?.to_owned();
        let dispatcher = Dispatcher::new(host, catalog, thread, config).map_err(|_| Error::Input)?;
        Ok(Self {
            port,
            dispatcher: Some(dispatcher),
            work: None,
            request: None,
            cancel: None,
            active: None,
            deferred: VecDeque::new(),
            budget,
            session: None,
            run: None,
            current_run: None,
            session_answer_pending: false,
        })
    }

    pub(crate) fn trace_deadline(&mut self) {
        if let Some((_, turn)) = &self.active {
            self.port.trace_timeout(turn);
        }
    }

    /// Surface the composite host's session requests to the run controller.
    #[must_use]
    pub fn with_session(
        mut self,
        requests: mpsc::Receiver<SessionRequest>,
        run: tokio::sync::watch::Sender<Option<Arc<()>>>,
    ) -> Self {
        self.session = Some(requests);
        self.run = Some(run);
        self
    }

    /// Poll PCM, process IO, absolute turn expiry and tool work fairly.
    /// Turn lifecycle remains observable after dispatch/cancellation accounting;
    /// an interrupted turn need not produce a realtime reply transcript.
    /// # Errors
    /// Bounded-queue overflow, invalid turn lifecycle, transport/media failure
    /// or refused output admission. The caller must await media/call cleanup.
    pub async fn next(&mut self) -> Result<MediaEvent, Error> {
        if self.work.is_none()
            && let Some(event) = self.deferred.pop_front()
        {
            return self.observed_control(event);
        }
        let deadline = self.dispatcher.as_ref().and_then(Dispatcher::deadline);
        tokio::select! {
            biased;
            () = expiry(deadline) => {
                if let Some(params) = self.dispatcher.as_mut().and_then(Dispatcher::expire) {
                    self.port.trace_timeout(&params.turn_id);
                    self.port.interrupt(&params, Instant::now() + Duration::from_secs(2))?;
                }
                Ok(other())
            },
            result = completed(&mut self.work) => {
                self.settle(result)?;
                Ok(other())
            },
            // The pending dispatch awaits this answer; never block it on media.
            request = requested(&mut self.session) => {
                self.session_answer_pending = true;
                Ok(MediaEvent::Control(Event::Session(request)))
            },
            // Keep nested real-media polling off the Windows main stack.
            event = Box::pin(self.port.next()) => match event? {
                MediaEvent::Control(event @ (Event::Tool { .. } | Event::TurnStarted { .. } | Event::TurnCompleted { .. })) => {
                    self.observed_control(event)
                },
                event => Ok(event),
            },
        }
    }

    fn observed_control(&mut self, event: Event) -> Result<MediaEvent, Error> {
        // Publish lifecycle only after the dispatcher has consumed it. Deferred
        // completion must still cancel pending effects before the caller sees it.
        let observed = match &event {
            Event::TurnStarted { thread, turn } => Event::TurnStarted {
                thread: thread.clone(),
                turn: turn.clone(),
            },
            Event::TurnCompleted { thread, turn, answered } => Event::TurnCompleted {
                thread: thread.clone(),
                turn: turn.clone(),
                answered: *answered,
            },
            _ => Event::Other,
        };
        let deferred = self.work.is_some();
        self.control(event)?;
        Ok(if deferred {
            other()
        } else {
            MediaEvent::Control(observed)
        })
    }

    fn control(&mut self, event: Event) -> Result<(), Error> {
        if self.work.is_some() {
            if self.deferred.len() == 32 {
                return Err(Error::Unavailable);
            }
            if let Event::TurnCompleted { thread, turn, .. } = &event
                && self.active.as_ref() == Some(&(thread.clone(), turn.clone()))
            {
                if let Some(cancel) = self.cancel.take() {
                    let _ = cancel.send(());
                }
                // Retire this turn before any queued request can execute.
                self.deferred.push_front(event);
            } else {
                self.deferred.push_back(event);
            }
            return Ok(());
        }
        match event {
            Event::TurnStarted { thread, turn } => {
                self.start_turn(&thread, &turn)?;
            }
            Event::TurnCompleted { thread, turn, .. } => {
                let dispatcher = self.dispatcher.as_mut().ok_or(Error::Unavailable)?;
                if dispatcher.finish_turn(&thread, &turn).is_ok() {
                    self.active = None;
                }
            }
            Event::Tool { id, mut call } => {
                if self.active.is_none()
                    && self
                        .dispatcher
                        .as_mut()
                        .ok_or(Error::Unavailable)?
                        .start_turn(&call.thread_id, &call.turn_id)
                        .is_ok()
                {
                    self.admit_turn(&call.thread_id, &call.turn_id)?;
                }
                crate::bridge_voice::normalize_skill_arguments(&mut call, self.session.is_some());
                let mut dispatcher = self.dispatcher.take().ok_or(Error::Unavailable)?;
                let (cancel, cancelled) = oneshot::channel();
                self.request = Some(id);
                self.cancel = Some(cancel);
                self.work = Some(Box::pin(async move {
                    let outcome = tokio::select! {
                        biased;
                        _ = cancelled => None,
                        outcome = Box::pin(dispatcher.dispatch(&call)) => Some(outcome),
                    };
                    (dispatcher, outcome)
                }));
            }
            _ => return Err(Error::Input),
        }
        Ok(())
    }

    fn start_turn(&mut self, thread: &str, turn: &str) -> Result<(), Error> {
        // A first tool request may precede the matching lifecycle notification.
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.0 == thread && active.1 == turn)
        {
            return Ok(());
        }
        let dispatcher = self.dispatcher.as_mut().ok_or(Error::Unavailable)?;
        dispatcher.start_turn(thread, turn).map_err(|_| Error::Unavailable)?;
        self.admit_turn(thread, turn)
    }

    fn admit_turn(&mut self, thread: &str, turn: &str) -> Result<(), Error> {
        self.active = Some((thread.into(), turn.into()));
        // Keep the turn's owner even when the next wake wins the device poll.
        if let Some(owner) = &self.run {
            owner.send_replace(self.current_run.clone());
        }
        if self.budget.admit_turn().is_err()
            && let Some(params) = self.dispatcher.as_mut().and_then(Dispatcher::refuse_budget)
        {
            self.port.interrupt(&params, Instant::now() + Duration::from_secs(2))?;
        }
        Ok(())
    }

    fn settle(&mut self, (dispatcher, outcome): (Dispatcher<H>, Option<Outcome>)) -> Result<(), Error> {
        self.work = None;
        self.session_answer_pending = false;
        self.cancel = None;
        self.dispatcher = Some(dispatcher);
        let id = self.request.take().ok_or(Error::Unavailable)?;
        let outcome = outcome.unwrap_or_else(|| Outcome {
            result: ToolResult {
                content_items: vec![ToolContent::InputText {
                    text: "tool request cancelled".into(),
                }],
                success: false,
            },
            interrupt: None,
        });
        self.port.answer_tool(&id, &outcome.result)?;
        if let Some(params) = outcome.interrupt {
            self.port.interrupt(&params, Instant::now() + Duration::from_secs(2))?;
        }
        Ok(())
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer, H: ToolHost + 'static> RetainedCall
    for ToolSession<'_, '_, R, W, P, H>
{
    async fn refresh_wake_time(&mut self, deadline: Instant, cancelled: impl Future<Output = ()>) -> Result<(), Error> {
        self.port.refresh_wake_time(deadline, cancelled).await
    }
}

impl<'p, 'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer, H: ToolHost + 'static> VoicePort
    for ToolSession<'p, 'a, R, W, P, H>
{
    fn send(&self, frame: PcmFrame) -> Result<(), Error> {
        self.port.send(frame)
    }
    fn try_send(&self, frame: PcmFrame) -> Result<bool, Error> {
        self.port.try_send(frame)
    }
    fn uplink_ready(&self) -> impl Future<Output = ()> + use<'p, 'a, R, W, P, H> {
        self.port.send_ready()
    }
    async fn next(&mut self) -> Result<MediaEvent, Error> {
        Box::pin(Self::next(self)).await
    }
    async fn ready_audio(&mut self) -> Result<Option<PcmFrame>, Error> {
        self.port.ready_audio().await
    }
    fn retire_audio(&mut self) {
        self.port.retire_audio();
    }
    fn rtp_lost(&self) -> u64 {
        self.port.dropped_frames().lost
    }
    fn bind_session(&mut self, run: Option<Arc<()>>) {
        self.current_run = run;
    }
    async fn flush_session_answer(&mut self) -> Result<(), Error> {
        if self.session_answer_pending {
            let result = completed(&mut self.work).await;
            self.settle(result)?;
        }
        self.port.flush_tool_answer().await
    }
}

async fn completed<H>(work: &mut Option<Work<H>>) -> (Dispatcher<H>, Option<Outcome>) {
    match work {
        Some(work) => work.await,
        None => std::future::pending().await,
    }
}

async fn requested(session: &mut Option<mpsc::Receiver<SessionRequest>>) -> SessionRequest {
    match session {
        Some(requests) => match requests.recv().await {
            Some(request) => request,
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

async fn expiry(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn other() -> MediaEvent {
    MediaEvent::Control(Event::Other)
}
