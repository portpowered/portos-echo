//! Projection of the existing voice admission owner.
use crate::bridge_voice::{Notice, ServeNotice, SessionNotice};
use echo_contracts::endpoint::{
    AssistantState, EndSessionOutcome, EndedReason, ReportState, SessionEnded, SessionOrigin,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use tokio::{
    sync::{Mutex, Notify, broadcast, mpsc, watch},
    time::{Duration, Instant},
};

const RECENT_ENDS: usize = 32;

tokio::task_local! {
    pub(super) static CURRENT: Voice;
}

#[derive(Clone)]
pub(crate) struct Voice {
    state: watch::Sender<View>,
    ended: broadcast::Sender<Ended>,
    starts: mpsc::Sender<super::remote_start::Start>,
    inbox: std::sync::Arc<Mutex<mpsc::Receiver<super::remote_start::Start>>>,
    new_start: std::sync::Arc<Notify>,
    pub(super) speaker: super::speaker::Control,
}

impl Default for Voice {
    fn default() -> Self {
        let (state, _) = watch::channel(View::default());
        let (ended, _) = broadcast::channel(RECENT_ENDS);
        let (starts, inbox) = mpsc::channel(1);
        Self {
            state,
            ended,
            starts,
            inbox: std::sync::Arc::new(Mutex::new(inbox)),
            new_start: std::sync::Arc::new(Notify::new()),
            speaker: super::speaker::Control::default(),
        }
    }
}

#[derive(Clone)]
struct Session {
    id: String,
    origin: SessionOrigin,
    state: AssistantState,
    ending: bool,
    user_ended: bool,
    admitted: bool,
}

#[derive(Clone)]
pub(super) struct Ended {
    session: Session,
    reason: EndedReason,
    at: std::time::SystemTime,
    pub(super) instant: Instant,
}

impl Ended {
    pub(super) fn payload<'a>(&'a self, time: &'a str) -> SessionEnded<'a> {
        SessionEnded {
            session_id: &self.session.id,
            origin: self.session.origin,
            reason: self.reason,
            ended_at: time,
        }
    }

    pub(super) fn time(&self) -> Result<String, crate::Error> {
        crate::bridge_voice::endpoint_timestamp(self.at)
    }
}

#[derive(Default, Clone)]
pub(super) struct View {
    ready: bool,
    disabled: bool,
    muted: bool,
    active: Option<Session>,
    recent: VecDeque<(String, EndedReason)>,
    pub(super) speaker: echo_link::SpeakerState,
}

impl View {
    pub(super) fn report(&self) -> ReportState<'_> {
        let active = self.active.as_ref().filter(|session| session.admitted);
        ReportState {
            state: if self.muted {
                AssistantState::Muted
            } else {
                active.map_or(AssistantState::Idle, |session| session.state)
            },
            session_id: active.map(|session| session.id.as_str()),
            origin: active.map(|session| session.origin),
            volume: self.speaker.volume,
            muted: self.speaker.muted,
        }
    }
}

impl Voice {
    pub(crate) fn current() -> Option<Self> {
        CURRENT.try_with(Clone::clone).ok()
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<View> {
        self.state.subscribe()
    }

    pub(crate) async fn with_speaker<T>(&self, client: &impl echo_link::EchoLink, work: impl Future<Output = T>) -> T {
        self.speaker.scope(client, &self.state, work).await
    }

    pub(super) fn terminal(&self) -> broadcast::Receiver<Ended> {
        self.ended.subscribe()
    }

    pub(super) async fn start(
        &self,
        payload: echo_contracts::endpoint::StartSession,
        received: Instant,
        lease: watch::Receiver<()>,
    ) -> Value {
        if let Some(refusal) = self.start_refusal() {
            return refusal;
        }
        let (start, reply) = super::remote_start::Start::new(payload, received, lease);
        let deadline = start.reply_deadline();
        if Instant::now() >= deadline || self.starts.try_send(start).is_err() {
            return unavailable();
        }
        self.new_start.notify_one();
        tokio::time::timeout_at(deadline, reply)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_else(unavailable)
    }

    pub(crate) async fn next_start(&self) -> super::remote_start::Start {
        match self.inbox.lock().await.recv().await {
            Some(start) => start,
            None => std::future::pending().await,
        }
    }

    pub(crate) fn begin_remote(&self, state: AssistantState) -> Result<String, crate::Error> {
        let id = echo_mcp_auth::new_request_id().map_err(|_| crate::Error::Unavailable)?;
        self.state.send_modify(|view| {
            view.active = Some(Session {
                id: id.clone(),
                origin: SessionOrigin::Remote,
                state,
                ending: false,
                user_ended: false,
                admitted: false,
            });
        });
        Ok(id)
    }

    pub(crate) fn remote_failed(&self) {
        self.finish(EndedReason::Error);
    }

    pub(crate) fn remote_admitted(&self, id: &str) {
        self.state.send_if_modified(|view| {
            if let Some(session) = view
                .active
                .as_mut()
                .filter(|session| session.id == id && !session.admitted)
            {
                session.admitted = true;
                return true;
            }
            false
        });
    }

    pub(crate) fn close_requested(&self) -> bool {
        self.state
            .borrow()
            .active
            .as_ref()
            .is_some_and(|session| session.ending)
    }

    pub(crate) fn close_finished(&self) {
        self.finish(EndedReason::Completed);
    }

    pub(crate) async fn start_waiting(&self) {
        loop {
            let notified = self.new_start.notified();
            if self.starts.capacity() == 0 {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn seed_mute(&self, muted: bool) {
        self.state.send_modify(|view| view.muted = muted);
    }

    pub(crate) fn unavailable(&self) {
        self.state.send_modify(|view| view.ready = false);
        self.finish(EndedReason::Error);
    }

    pub(crate) fn observe(&self, notice: &ServeNotice) -> Result<(), crate::Error> {
        match notice {
            ServeNotice::Ready => self.state.send_modify(|view| view.ready = true),
            ServeNotice::Listening | ServeNotice::Session(SessionNotice::Started) => {
                if self.state.borrow().active.is_none() {
                    let id = echo_mcp_auth::new_request_id().map_err(|_| crate::Error::Unavailable)?;
                    self.state.send_modify(|view| {
                        view.active = Some(Session {
                            id,
                            origin: SessionOrigin::Wake,
                            state: AssistantState::Listening,
                            ending: false,
                            user_ended: false,
                            admitted: true,
                        });
                    });
                }
            }
            ServeNotice::Kill(disabled) => self.state.send_modify(|view| view.disabled = *disabled),
            ServeNotice::Device(echo_link::LinkEvent::Mute(muted))
            | ServeNotice::Session(SessionNotice::Run(Notice::Device(echo_link::LinkEvent::Mute(muted)))) => {
                self.state.send_modify(|view| view.muted = *muted);
            }
            ServeNotice::Session(SessionNotice::Run(Notice::Thinking)) => self.phase(AssistantState::Thinking),
            ServeNotice::Session(SessionNotice::Run(Notice::ReplyStarted)) => self.phase(AssistantState::Speaking),
            ServeNotice::Session(SessionNotice::Run(Notice::ReplyPlayed)) => self.phase(AssistantState::Listening),
            ServeNotice::Unanswered { reason } => {
                self.finish(if *reason == "no_response" {
                    EndedReason::Timeout
                } else {
                    EndedReason::Error
                });
            }
            ServeNotice::Session(SessionNotice::Completed(_)) => self.finish(EndedReason::Completed),
            ServeNotice::Session(SessionNotice::Run(Notice::SessionTool(receipt)))
                if receipt.tool == "end_session" && receipt.outcome == "applied" =>
            {
                self.state.send_modify(|view| {
                    if let Some(active) = &mut view.active {
                        active.user_ended = true;
                    }
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn phase(&self, state: AssistantState) {
        self.state.send_if_modified(|view| {
            let Some(active) = &mut view.active else { return false };
            if active.state == state {
                return false;
            }
            active.state = state;
            true
        });
    }

    pub(crate) fn start_refusal(&self) -> Option<Value> {
        let view = self.state.borrow();
        if view.muted {
            return Some(super::commands::refusal("MUTED", "The microphone is muted."));
        }
        if view.disabled {
            return Some(super::commands::refusal("DISABLED", "The voice assistant is disabled."));
        }
        if let Some(active) = &view.active {
            return Some(json!({
                "ok": false,
                "failure": {"code":"BUSY", "message":"A voice session is already active."},
                "sessionId": active.id,
                "origin": active.origin,
            }));
        }
        (!view.ready).then(unavailable)
    }

    fn finish(&self, reason: EndedReason) {
        let mut ended = None;
        self.state.send_if_modified(|view| {
            let Some(session) = view.active.take() else {
                return false;
            };
            if !session.admitted {
                return true;
            }
            let reason = match reason {
                EndedReason::Completed if session.ending => EndedReason::Remote,
                EndedReason::Completed if session.user_ended => EndedReason::User,
                _ if view.muted => EndedReason::Muted,
                _ => reason,
            };
            if view.recent.len() == RECENT_ENDS {
                view.recent.pop_front();
            }
            view.recent.push_back((session.id.clone(), reason));
            ended = Some(Ended {
                session,
                reason,
                at: crate::bridge_time::now(),
                instant: Instant::now(),
            });
            true
        });
        if let Some(ended) = ended {
            let _ = self.ended.send(ended);
        }
    }

    pub(super) async fn end(&self, id: &str, received: Instant) -> Value {
        let mut states = self.subscribe();
        let mut found = false;
        self.state.send_if_modified(|view| {
            let Some(active) = view.active.as_mut().filter(|session| session.id == id) else {
                return false;
            };
            found = true;
            !std::mem::replace(&mut active.ending, true)
        });
        if let Some(result) = ended(&states.borrow(), id, found) {
            return result;
        }
        let result = states.wait_for(|view| view.active.as_ref().is_none_or(|session| session.id != id));
        match tokio::time::timeout_at(received + Duration::from_secs(1), result).await {
            Ok(Ok(view)) => ended(&view, id, true).unwrap_or_else(unavailable),
            _ => unavailable(),
        }
    }

    pub(crate) async fn ending(&self) {
        let mut states = self.subscribe();
        let _ = states
            .wait_for(|view| view.active.as_ref().is_some_and(|session| session.ending))
            .await;
    }
}

fn ended(view: &View, id: &str, found: bool) -> Option<Value> {
    if let Some((_, reason)) = view.recent.iter().find(|(session, _)| session == id) {
        return Some(if !found || *reason == EndedReason::Remote {
            json!(EndSessionOutcome::closed())
        } else {
            unavailable()
        });
    }
    (!found).then(not_found)
}

pub(super) fn not_found() -> Value {
    super::commands::refusal("NOT_FOUND", "The voice session was not found.")
}

pub(crate) async fn ending() {
    if let Some(voice) = Voice::current() {
        voice.ending().await;
    } else {
        std::future::pending::<()>().await;
    }
}

pub(crate) fn close_requested() -> bool {
    Voice::current().is_some_and(|voice| voice.close_requested())
}

pub(crate) async fn start_waiting() {
    if let Some(voice) = Voice::current() {
        voice.start_waiting().await;
    } else {
        std::future::pending::<()>().await;
    }
}

pub(crate) fn unavailable() -> Value {
    super::commands::refusal("NOT_READY", "The voice provider is unavailable.")
}
