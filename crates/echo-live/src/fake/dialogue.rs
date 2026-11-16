//! Additive synthetic dialogue. No captured-provider or acoustic claims.
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

use crate::call::{CallError, drive, now};
use crate::{AudioFrame, LivePeer, fake_http};

mod state;
use state::Dialogue;
#[cfg(test)]
mod tests;

const MAX_CALLS: usize = 4;

/// One explicit initial delegation and one reply after an exact commentary match.
/// Text is synthetic, bounded, and never written to a production trace.
#[derive(Clone)]
pub struct DialogueScript {
    pub delegation_id: String,
    pub input: String,
    pub commentary: String,
    pub transcript: String,
    /// 1..=20 valid 20-ms Opus comfort-noise frames, not speech synthesis.
    pub audio_frames: u16,
}

/// Ordered successful sends, retained with their containing call/delegation identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogueSent {
    Text(String),
    Audio(AudioFrame),
}

/// `call_index` identifies this fake's actual accepted signaling call, not a model call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogueCall {
    pub call_index: usize,
    pub delegation_id: String,
    pub sent: Vec<DialogueSent>,
    pub rejected_commentary: u16,
    pub received_audio: u16,
    pub reply_completed: bool,
    pub closed: bool,
    /// Sanitized failure category; no HTTP bodies, credentials, SDP or input text.
    pub failure: Option<String>,
}

#[derive(Clone, Default)]
pub struct DialogueObserver {
    calls: Arc<Mutex<Vec<DialogueCall>>>,
    changed: Arc<tokio::sync::Notify>,
}

impl DialogueObserver {
    /// Snapshot of bounded actual sends, including partial sends on failure.
    /// # Errors
    /// Returns Signaling if an observer lock was poisoned.
    pub fn calls(&self) -> Result<Vec<DialogueCall>, CallError> {
        Ok(self.calls.lock().map_err(|_| CallError::Signaling)?.clone())
    }

    /// Wait on an actual observation, with no fixed synchronization delay.
    /// The caller owns the deadline; cancellation owns no background task.
    /// # Errors
    /// Returns Signaling if an observer lock was poisoned.
    pub async fn wait_for(&self, index: usize, predicate: impl Fn(&DialogueCall) -> bool) -> Result<(), CallError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.calls()?.get(index).is_some_and(&predicate) {
                return Ok(());
            }
            changed.await;
        }
    }

    fn record(&self, id: &str) -> Result<usize, CallError> {
        let mut calls = self.calls.lock().map_err(|_| CallError::Signaling)?;
        if calls.len() == MAX_CALLS {
            return Err(CallError::Signaling);
        }
        let index = calls.len();
        calls.push(DialogueCall {
            call_index: index,
            delegation_id: id.into(),
            sent: vec![],
            rejected_commentary: 0,
            received_audio: 0,
            reply_completed: false,
            closed: false,
            failure: None,
        });
        self.changed.notify_waiters();
        Ok(index)
    }

    fn update(&self, index: usize, update: impl FnOnce(&mut DialogueCall)) -> Result<(), CallError> {
        let mut calls = self.calls.lock().map_err(|_| CallError::Signaling)?;
        update(calls.get_mut(index).ok_or(CallError::Signaling)?);
        self.changed.notify_waiters();
        Ok(())
    }
}

/// Owns the listener and at most four lifetime connection tasks/observations.
/// Explicit shutdown joins every task; Drop aborts the owner and its `JoinSet`.
pub struct DialogueLive {
    base: String,
    task: JoinHandle<()>,
    observer: DialogueObserver,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl DialogueLive {
    pub(super) async fn bind(script: DialogueScript) -> Result<Self, CallError> {
        script.validate()?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| CallError::Signaling)?;
        let port = listener.local_addr().map_err(|_| CallError::Signaling)?.port();
        let mut base = reqwest::Url::parse("http://127.0.0.1").map_err(|_| CallError::Configuration)?;
        base.set_port(Some(port)).map_err(|()| CallError::Configuration)?;
        let observer = DialogueObserver::default();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(listen(listener, script, observer.clone(), stopped));
        Ok(Self {
            base: base.to_string().trim_end_matches('/').into(),
            task,
            observer,
            stop: Some(stop),
        })
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base
    }

    #[must_use]
    pub fn observer(&self) -> DialogueObserver {
        self.observer.clone()
    }

    /// Stop accepting and join every owned connection, including stalled HTTP.
    /// # Errors
    /// Returns Signaling if the listener owner panicked or was cancelled.
    pub async fn shutdown(mut self) -> Result<(), CallError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.map_err(|_| CallError::Signaling)
    }
}

impl Drop for DialogueLive {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn listen(
    listener: TcpListener,
    script: DialogueScript,
    observer: DialogueObserver,
    mut stopped: tokio::sync::oneshot::Receiver<()>,
) {
    let mut tasks = JoinSet::new();
    let mut admitted = 0;
    loop {
        tokio::select! {
            _ = &mut stopped => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { break; };
                if admitted < MAX_CALLS {
                    admitted += 1;
                    tasks.spawn(serve(stream, script.clone(), observer.clone()));
                }
            }
            _ = tasks.join_next(), if !tasks.is_empty() => (),
        }
    }
    tasks.shutdown().await;
    if let Ok(mut calls) = observer.calls.lock() {
        for call in calls.iter_mut().filter(|call| !call.closed) {
            call.closed = true;
            if !call.reply_completed {
                call.failure = Some("cancelled".into());
            }
        }
    }
    observer.changed.notify_waiters();
}

async fn serve(mut stream: TcpStream, script: DialogueScript, observer: DialogueObserver) {
    // Record admission before reading so partial HTTP and cancellation stay visible.
    let Ok(index) = observer.record(&script.delegation_id) else {
        return;
    };
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let request = fake_http::request(&mut stream).await?;
        let offer = request.body["sdp"].as_str().ok_or(CallError::Decode)?;
        let mut peer = Box::pin(LivePeer::bind((Ipv4Addr::LOCALHOST, 0).into(), now())).await?;
        let answer = peer.answer(offer).await?;
        fake_http::respond(&mut stream, 200, &answer, None).await?;
        let result = replay(&mut peer, script, &observer, index).await;
        peer.close();
        result
    })
    .await
    .unwrap_or(Err(CallError::Deadline));
    let _ = observer.update(index, |call| {
        call.closed = true;
        call.failure = result.err().map(|error| error.to_string());
    });
}

async fn replay(
    peer: &mut LivePeer,
    script: DialogueScript,
    observer: &DialogueObserver,
    index: usize,
) -> Result<(), CallError> {
    let deadline = now() + Duration::from_secs(20);
    while !peer.ready() {
        drive(peer, deadline).await?;
    }
    let initial = json!({"type":"session.delegation.created", "delegation": {
        "id":script.delegation_id, "target":"client", "input":script.input
    }})
    .to_string();
    send(peer, observer, index, DialogueSent::Text(initial)).await?;
    let mut dialogue = Dialogue::new(script);
    loop {
        drive(peer, deadline).await?;
        while let Some(text) = peer.pop_text() {
            let matched = dialogue.commentary(&text)?;
            observer.update(index, |call| call.rejected_commentary = dialogue.rejected())?;
            if matched {
                for output in dialogue.reply() {
                    send(peer, observer, index, output).await?;
                }
                observer.update(index, |call| call.reply_completed = true)?;
            }
        }
        while peer.pop_audio().is_some() {
            dialogue.microphone()?;
            observer.update(index, |call| call.received_audio += 1)?;
        }
    }
}

async fn send(
    peer: &mut LivePeer,
    observer: &DialogueObserver,
    index: usize,
    output: DialogueSent,
) -> Result<(), CallError> {
    match &output {
        DialogueSent::Text(text) => peer.send_text(text).await?,
        DialogueSent::Audio(frame) => peer.send_audio(frame.clone(), now()).await?,
    }
    observer.update(index, |call| call.sent.push(output))
}
