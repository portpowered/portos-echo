//! Caller-driven tool work shares the media owner's single process reader.
use super::{Event, MediaCall, MediaEvent};
use crate::{Error, bridge_voice::VoicePort};
use echo_agent_api::ToolHost;
use echo_codex::codex_protocol::{RequestId, ToolContent, ToolResult};
use echo_config::AgentConfig;
use echo_delegation::codex_dispatch::{Catalog, Dispatcher, Outcome};
use echo_media::{MediaPeer, PcmFrame};
use std::{collections::VecDeque, future::Future, pin::Pin, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::oneshot,
    time::Instant,
};

type Work<H> = Pin<Box<dyn Future<Output = (Dispatcher<H>, Option<Outcome>)>>>;

/// One connected thread, one dispatcher and one bounded pending attempt.
/// The future is retained across cancelled `next` polls; no task is detached.
/// Turn completion cancels unfinished work before admitting the next turn.
/// Confirmation and MCP authorization remain exclusively at the supplied host.
/// The caller must admit the daily cap before model work and advertise this
/// catalog when starting the call. This owner cannot undo model admission.
pub struct ToolSession<'p, 'a, R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer, H> {
    port: &'p mut MediaCall<'a, R, W, P>,
    dispatcher: Option<Dispatcher<H>>,
    work: Option<Work<H>>,
    request: Option<RequestId>,
    cancel: Option<oneshot::Sender<()>>,
    active: Option<(String, String)>,
    deferred: VecDeque<Event>,
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
        })
    }

    /// Poll PCM, process IO, absolute turn expiry and tool work fairly.
    /// # Errors
    /// Bounded-queue overflow, invalid turn lifecycle, transport/media failure
    /// or refused output admission. The caller must await media/call cleanup.
    pub async fn next(&mut self) -> Result<MediaEvent, Error> {
        if self.work.is_none()
            && let Some(event) = self.deferred.pop_front()
        {
            self.control(event)?;
            return Ok(other());
        }
        let deadline = self.dispatcher.as_ref().and_then(Dispatcher::deadline);
        tokio::select! {
            biased;
            () = expiry(deadline) => {
                if let Some(params) = self.dispatcher.as_mut().and_then(Dispatcher::expire) {
                    self.port.interrupt(&params, Instant::now() + Duration::from_secs(2))?;
                }
                Ok(other())
            },
            result = completed(&mut self.work) => {
                self.settle(result)?;
                Ok(other())
            },
            event = self.port.next() => match event? {
                MediaEvent::Control(event @ (Event::Tool { .. } | Event::TurnStarted { .. } | Event::TurnCompleted { .. })) => {
                    self.control(event)?;
                    Ok(other())
                },
                event => Ok(event),
            },
        }
    }

    fn control(&mut self, event: Event) -> Result<(), Error> {
        if self.work.is_some() {
            if self.deferred.len() == 32 {
                return Err(Error::Unavailable);
            }
            if let Event::TurnCompleted { thread, turn } = &event
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
        let dispatcher = self.dispatcher.as_mut().ok_or(Error::Unavailable)?;
        match event {
            Event::TurnStarted { thread, turn } => {
                dispatcher.start_turn(&thread, &turn).map_err(|_| Error::Unavailable)?;
                self.active = Some((thread, turn));
            }
            Event::TurnCompleted { thread, turn } => {
                if dispatcher.finish_turn(&thread, &turn).is_ok() {
                    self.active = None;
                }
            }
            Event::Tool { id, call } => {
                let mut dispatcher = self.dispatcher.take().ok_or(Error::Unavailable)?;
                let (cancel, cancelled) = oneshot::channel();
                self.request = Some(id);
                self.cancel = Some(cancel);
                self.work = Some(Box::pin(async move {
                    let outcome = tokio::select! {
                        biased;
                        _ = cancelled => None,
                        outcome = dispatcher.dispatch(&call) => Some(outcome),
                    };
                    (dispatcher, outcome)
                }));
            }
            _ => return Err(Error::Input),
        }
        Ok(())
    }

    fn settle(&mut self, (dispatcher, outcome): (Dispatcher<H>, Option<Outcome>)) -> Result<(), Error> {
        self.work = None;
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

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, P: MediaPeer, H: ToolHost + 'static> VoicePort
    for ToolSession<'_, '_, R, W, P, H>
{
    fn send(&self, frame: PcmFrame) -> Result<(), Error> {
        self.port.send(frame)
    }
    async fn next(&mut self) -> Result<MediaEvent, Error> {
        Self::next(self).await
    }
    async fn ready_audio(&mut self) -> Result<Option<PcmFrame>, Error> {
        self.port.ready_audio().await
    }
}

async fn completed<H>(work: &mut Option<Work<H>>) -> (Dispatcher<H>, Option<Outcome>) {
    match work {
        Some(work) => work.await,
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
