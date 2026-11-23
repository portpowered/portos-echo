use crate::screen_mouth::Samples;
use echo_contracts::secret::Secret;
use echo_screen_contract::mouth::MouthSample;
use echo_screen_contract::{ScreenFrame, UiState};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, watch};

#[derive(Clone)]
pub(crate) enum Publication {
    Frame(String),
    PackReload(String),
    Mouth(Samples),
}

impl Publication {
    pub fn event(self) -> axum::response::sse::Event {
        let event = axum::response::sse::Event::default();
        match self {
            Self::Frame(data) => event.data(data),
            Self::PackReload(data) => event.event("screen-pack-reload").data(data),
            Self::Mouth(samples) => event.event("mouth").data(samples.json()),
        }
    }

    #[cfg(test)]
    pub fn name(&self) -> Option<&'static str> {
        match self {
            Self::Frame(_) => None,
            Self::PackReload(_) => Some("screen-pack-reload"),
            Self::Mouth(_) => Some("mouth"),
        }
    }

    #[cfg(test)]
    pub fn data(&self) -> String {
        match self {
            Self::Frame(data) | Self::PackReload(data) => data.clone(),
            Self::Mouth(samples) => samples.json(),
        }
    }
}

pub(crate) struct Client {
    pub frames: mpsc::Receiver<Publication>,
    pub cancelled: watch::Receiver<bool>,
}

struct Writer {
    frames: mpsc::Sender<Publication>,
    cancel: watch::Sender<bool>,
    /// The client asked for lip-sync batches on this stream.
    mouth: bool,
}

/// Queue slots a lip-sync batch never takes: screen frames keep them.
const MOUTH_RESERVE: usize = 24;

#[derive(PartialEq, Eq)]
pub(crate) enum PackObservation {
    Unobserved,
    Current(Option<(String, [u8; 32])>),
}

pub(crate) struct Core {
    digest: Secret<[u8; 32]>,
    revoked: watch::Sender<bool>,
    draining: bool,
    seq: u64,
    pub pack_revision: u64,
    pub pack_publication: Option<([u8; 32], Option<String>)>,
    pub pack_identity: PackObservation,
    current: Option<String>,
    clients: Vec<Writer>,
}

impl Core {
    pub fn new(token: &Secret<String>) -> Self {
        Self {
            digest: Secret::new(Sha256::digest(token.expose().as_bytes()).into()),
            revoked: watch::channel(false).0,
            draining: false,
            seq: 0,
            pack_revision: 0,
            pack_identity: PackObservation::Unobserved,
            pack_publication: None,
            current: None,
            clients: Vec::new(),
        }
    }

    pub fn authorized(&self, token: &str) -> bool {
        let digest = Sha256::digest(token.as_bytes());
        let difference = digest
            .iter()
            .zip(self.digest.expose())
            .fold(0, |sum, (a, b)| sum | (a ^ b));
        !*self.revoked.borrow() && !token.is_empty() && difference == 0
    }

    pub fn revoke(&mut self) {
        self.revoked.send_replace(true);
        self.close_clients();
    }

    pub fn revocation(&self) -> watch::Receiver<bool> {
        self.revoked.subscribe()
    }

    pub(crate) fn camera_key(&self) -> Option<&[u8; 32]> {
        self.accepting().then_some(self.digest.expose())
    }

    pub fn drain(&mut self) {
        self.draining = true;
    }

    pub fn accepting(&self) -> bool {
        !self.draining && !*self.revoked.borrow()
    }

    fn close_clients(&mut self) {
        for client in self.clients.drain(..) {
            client.cancel.send_replace(true);
        }
    }

    #[cfg(test)]
    pub fn attach(&mut self, cancel: watch::Sender<bool>) -> Option<Client> {
        self.attach_with(cancel, false)
    }

    pub fn attach_with(&mut self, cancel: watch::Sender<bool>, mouth: bool) -> Option<Client> {
        if self.draining {
            return None;
        }
        self.clients.retain(|client| !client.frames.is_closed());
        if self.clients.len() == 32 {
            return None;
        }
        let (tx, frames) = mpsc::channel(32);
        let cancelled = cancel.subscribe();
        if let Some(current) = &self.current {
            let _ = tx.try_send(Publication::Frame(current.clone()));
        }
        self.clients.push(Writer {
            frames: tx,
            cancel,
            mouth,
        });
        Some(Client { frames, cancelled })
    }

    pub fn publish(&mut self, state: UiState) -> bool {
        let Some(seq) = self.seq.checked_add(1) else {
            return false;
        };
        self.seq = seq;
        let Ok(wire) = serde_json::to_string(&ScreenFrame { seq, state }) else {
            return false;
        };
        self.current = Some(wire.clone());
        if self.draining {
            return true;
        }
        self.broadcast(&Publication::Frame(wire));
        true
    }

    pub fn pack_reload(&mut self, active: Option<&str>) -> bool {
        if !self.accepting() || self.pack_revision == 9_007_199_254_740_991 {
            return false;
        }
        self.pack_revision += 1;
        self.broadcast(&Publication::PackReload(
            serde_json::json!({"version":1,"revision":self.pack_revision,"active":active}).to_string(),
        ));
        true
    }

    /// Some open stream asked for lip sync.
    pub fn mouth_wanted(&self) -> bool {
        self.accepting()
            && self
                .clients
                .iter()
                .any(|client| client.mouth && !client.frames.is_closed())
    }

    /// Offer one lip-sync batch to the streams that asked for it. It is worth
    /// nothing late: a stream with a backlog misses the batch and stays open,
    /// and the batch never takes a slot a screen frame may need. Returns the
    /// streams that took it. The frame sequence and snapshot are untouched.
    pub fn offer_mouth_samples(&self, sent_ms: u64, samples: &[MouthSample]) -> usize {
        let publication = Publication::Mouth(Samples::new(sent_ms, samples));
        self.clients
            .iter()
            .filter(|client| client.mouth && client.frames.capacity() > MOUTH_RESERVE)
            .filter(|client| client.frames.try_send(publication.clone()).is_ok())
            .count()
    }

    fn broadcast(&mut self, publication: &Publication) {
        self.clients.retain(|client| {
            if client.frames.try_send(publication.clone()).is_ok() {
                true
            } else {
                client.cancel.send_replace(true);
                false
            }
        });
    }
}

pub(crate) async fn pump(
    core: Arc<Mutex<Core>>,
    mut states: mpsc::Receiver<UiState>,
    mut requests: mpsc::Receiver<tokio::sync::oneshot::Sender<()>>,
    stop: watch::Sender<bool>,
) {
    let stop_rx = stop.subscribe();
    let mut requests_open = true;
    loop {
        tokio::select! {
            biased;
            () = crate::cancelled(stop_rx.clone()) => break,
            request = requests.recv(), if requests_open => {
                let Some(ack) = request else { requests_open = false; continue; };
                // Snapshot a finite FIFO prefix: concurrent producers cannot prolong this barrier.
                let prefix = states.len();
                let mut current = core.lock().await;
                if *stop.borrow() { break; }
                let mut committed = true;
                for _ in 0..prefix {
                    if !states.try_recv().is_ok_and(|state| current.publish(state)) {
                        committed = false;
                        break;
                    }
                }
                if !committed || *stop.borrow() { break; }
                let _ = ack.send(());
            }
            state = states.recv() => {
                let Some(state) = state else { break; };
                let mut current = core.lock().await;
                if *stop.borrow() || !current.publish(state) { break; }
            }
        }
    }
    core.lock().await.revoke();
    stop.send_replace(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    #[tokio::test]
    async fn drain_preserves_queued_failure_while_revocation_cancels() {
        for graceful in [true, false] {
            let mut core = Core::new(&Secret::new("synthetic-screen-pair".into()));
            let (cancel, _) = watch::channel(false);
            let mut client = core.attach(cancel.clone()).unwrap();
            let states = [
                UiState::Listening,
                UiState::Error {
                    message: "unavailable".into(),
                    relink: false,
                },
            ];
            for state in &states {
                assert!(core.publish(state.clone()));
            }
            if graceful {
                core.drain();
                client.frames.close();
            } else {
                core.revoke();
            }
            assert_eq!(core.authorized("synthetic-screen-pair"), graceful);
            if graceful {
                assert!(core.attach(watch::channel(false).0).is_none());
            }
            assert_eq!(*client.cancelled.borrow(), !graceful);
            for (index, state) in states.into_iter().enumerate() {
                let frame: ScreenFrame = serde_json::from_str(&client.frames.recv().await.unwrap().data()).unwrap();
                assert_eq!(
                    frame,
                    ScreenFrame {
                        seq: index as u64 + 1,
                        state
                    }
                );
            }
            assert!(client.frames.recv().await.is_none());
        }
    }

    #[tokio::test]
    async fn held_pump_commits_fifo_before_ack_and_first_snapshot() {
        let core = Arc::new(Mutex::new(Core::new(&Secret::new("synthetic-screen-pair".into()))));
        let (states, input) = mpsc::channel(32);
        let (requests, receipt) = mpsc::channel(32);
        let (stop, _) = watch::channel(false);
        let mut held = core.lock().await;
        assert!(held.publish(UiState::Listening));
        states.try_send(UiState::Connected).unwrap();
        states
            .try_send(UiState::Error {
                message: "relink".into(),
                relink: true,
            })
            .unwrap();
        let (ack, mut done) = tokio::sync::oneshot::channel();
        requests.try_send(ack).unwrap();
        let pump = pump(core.clone(), input, receipt, stop.clone());
        tokio::pin!(pump);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(pump.as_mut().poll(cx).is_pending())).await);
        assert!(matches!(
            done.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        let mut stale = held.attach(watch::channel(false).0).unwrap();
        let stale: ScreenFrame = serde_json::from_str(&stale.frames.recv().await.unwrap().data()).unwrap();
        assert_eq!(stale.state, UiState::Listening); // Queue admission alone reproduces the old race.
        drop(held);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(pump.as_mut().poll(cx).is_pending())).await);
        assert_eq!(done.try_recv(), Ok(()));
        let mut late = core.lock().await.attach(watch::channel(false).0).unwrap();
        let frame: ScreenFrame = serde_json::from_str(&late.frames.recv().await.unwrap().data()).unwrap();
        assert_eq!(frame.seq, 3);
        assert_eq!(
            frame.state,
            UiState::Error {
                message: "relink".into(),
                relink: true
            }
        );
        stop.send_replace(true);
        pump.await;
    }

    #[tokio::test]
    async fn stopped_or_failed_pump_never_acknowledges() {
        for stopped in [true, false] {
            let core = Arc::new(Mutex::new(Core::new(&Secret::new("synthetic-screen-pair".into()))));
            let (states, input) = mpsc::channel(1);
            let (requests, receipt) = mpsc::channel(1);
            let (stop, _) = watch::channel(false);
            let mut held = core.lock().await;
            held.seq = u64::MAX;
            states.try_send(UiState::Muted).unwrap();
            let (ack, mut done) = tokio::sync::oneshot::channel();
            requests.try_send(ack).unwrap();
            let pump = pump(core.clone(), input, receipt, stop.clone());
            tokio::pin!(pump);
            assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(pump.as_mut().poll(cx).is_pending())).await);
            if stopped {
                stop.send_replace(true);
            }
            drop(held);
            pump.await;
            assert!(matches!(
                done.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ));
            assert!(*stop.borrow());
        }
    }

    #[tokio::test]
    async fn cancelled_request_and_closed_request_channel_keep_state_pump_live() {
        let core = Arc::new(Mutex::new(Core::new(&Secret::new("synthetic-screen-pair".into()))));
        let (states, input) = mpsc::channel(1);
        let (requests, receipt) = mpsc::channel(1);
        let (stop, _) = watch::channel(false);
        let (ack, done) = tokio::sync::oneshot::channel();
        drop(done);
        requests.try_send(ack).unwrap();
        drop(requests);
        states.try_send(UiState::Muted).unwrap();
        let pump = pump(core.clone(), input, receipt, stop.clone());
        tokio::pin!(pump);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(pump.as_mut().poll(cx).is_pending())).await);
        let mut client = core.lock().await.attach(watch::channel(false).0).unwrap();
        let frame: ScreenFrame = serde_json::from_str(&client.frames.recv().await.unwrap().data()).unwrap();
        assert_eq!(frame.state, UiState::Muted);
        states.try_send(UiState::Connected).unwrap();
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(pump.as_mut().poll(cx).is_pending())).await);
        let frame: ScreenFrame = serde_json::from_str(&client.frames.recv().await.unwrap().data()).unwrap();
        assert_eq!(frame.state, UiState::Connected);
        stop.send_replace(true);
        pump.await;
    }

    #[tokio::test]
    async fn bounded_stalled_writer_and_snapshot_sequence() {
        let mut core = Core::new(&Secret::new("synthetic-screen-pair".into()));
        let mut stalled = core.attach(watch::channel(false).0).unwrap();
        let mut healthy = core.attach(watch::channel(false).0).unwrap();
        assert!(healthy.frames.try_recv().is_err());
        for seq in 1..=33 {
            assert!(core.publish(UiState::Connected));
            let wire = healthy.frames.recv().await.unwrap();
            assert_eq!(
                serde_json::from_str::<ScreenFrame>(&wire.data()).unwrap(),
                ScreenFrame {
                    seq,
                    state: UiState::Connected
                }
            );
        }
        assert!(*stalled.cancelled.borrow());
        assert_eq!(stalled.frames.len(), 32);
        assert_eq!(core.clients.len(), 1);
        let mut late = core.attach(watch::channel(false).0).unwrap();
        assert_eq!(
            serde_json::from_str::<ScreenFrame>(&late.frames.recv().await.unwrap().data())
                .unwrap()
                .seq,
            33
        );
        core.seq = u64::MAX - 1;
        assert!(core.publish(UiState::Muted));
        assert_eq!(
            serde_json::from_str::<ScreenFrame>(&healthy.frames.recv().await.unwrap().data())
                .unwrap()
                .seq,
            u64::MAX
        );
        assert!(!core.publish(UiState::Ready {
            message: "must not wrap".into()
        }));
        core.revoke();
        assert!(!core.authorized("synthetic-screen-pair"));
        assert!(*healthy.cancelled.borrow());
        assert!(healthy.frames.recv().await.is_none());
        stalled.frames.close();
    }

    #[test]
    fn authentication_and_stream_slots() {
        let mut core = Core::new(&Secret::new("synthetic-screen-pair".into()));
        assert!(core.authorized("synthetic-screen-pair"));
        assert!(!core.authorized(""));
        assert!(!core.authorized("synthetic-screen-pairs"));
        let mut clients = Vec::new();
        for _ in 0..32 {
            clients.push(core.attach(watch::channel(false).0).unwrap());
        }
        assert!(core.attach(watch::channel(false).0).is_none());
        clients.pop();
        assert!(core.attach(watch::channel(false).0).is_some());
        core.revoke();
        assert!(clients.iter().all(|client| *client.cancelled.borrow()));
    }

    #[tokio::test]
    async fn pack_reload_is_named_bounded_and_independent_of_frame_snapshot() {
        let mut core = Core::new(&Secret::new("synthetic-screen-pair".into()));
        let mut client = core.attach(watch::channel(false).0).unwrap();
        assert!(core.publish(UiState::Listening));
        assert_eq!(client.frames.recv().await.unwrap().name(), None);
        assert!(core.pack_reload(Some("local")));
        let message = client.frames.recv().await.unwrap();
        assert_eq!(message.name(), Some("screen-pack-reload"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&message.data()).unwrap(),
            serde_json::json!({"version":1,"revision":1,"active":"local"})
        );
        let mut late = core.attach(watch::channel(false).0).unwrap();
        let snapshot = late.frames.recv().await.unwrap();
        assert_eq!(snapshot.name(), None);
        assert_eq!(serde_json::from_str::<ScreenFrame>(&snapshot.data()).unwrap().seq, 1);
        assert!(late.frames.try_recv().is_err());
        core.pack_revision = 9_007_199_254_740_990;
        assert!(core.pack_reload(None));
        assert_eq!(core.pack_revision, 9_007_199_254_740_991);
        assert!(!core.pack_reload(None));
        assert!(core.publish(UiState::Muted));
        assert_eq!(core.seq, 2);
        core.drain();
        assert!(!core.pack_reload(Some("local")));
        core.revoke();
        assert!(!core.pack_reload(None));
    }
}
