use echo_contracts::secret::Secret;
use echo_screen_contract::{ScreenFrame, UiState};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, watch};

pub(crate) struct Client {
    pub frames: mpsc::Receiver<String>,
    pub cancelled: watch::Receiver<bool>,
}

struct Writer {
    frames: mpsc::Sender<String>,
    cancel: watch::Sender<bool>,
}

pub(crate) struct Core {
    digest: Secret<[u8; 32]>,
    revoked: bool,
    seq: u64,
    current: Option<String>,
    clients: Vec<Writer>,
}

impl Core {
    pub fn new(token: &Secret<String>) -> Self {
        Self {
            digest: Secret::new(Sha256::digest(token.expose().as_bytes()).into()),
            revoked: false,
            seq: 0,
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
        !self.revoked && !token.is_empty() && difference == 0
    }

    pub fn revoke(&mut self) {
        self.revoked = true;
        self.close_clients();
    }

    fn close_clients(&mut self) {
        for client in self.clients.drain(..) {
            client.cancel.send_replace(true);
        }
    }

    pub fn attach(&mut self, cancel: watch::Sender<bool>) -> Option<Client> {
        self.clients.retain(|client| !client.frames.is_closed());
        if self.clients.len() == 32 {
            return None;
        }
        let (tx, frames) = mpsc::channel(32);
        let cancelled = cancel.subscribe();
        if let Some(current) = &self.current {
            let _ = tx.try_send(current.clone());
        }
        self.clients.push(Writer { frames: tx, cancel });
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
        self.clients.retain(|client| {
            if client.frames.try_send(wire.clone()).is_ok() {
                true
            } else {
                client.cancel.send_replace(true);
                false
            }
        });
        true
    }
}

pub(crate) async fn pump(core: Arc<Mutex<Core>>, mut states: mpsc::Receiver<UiState>, stop: watch::Sender<bool>) {
    let stop_rx = stop.subscribe();
    loop {
        tokio::select! {
            biased;
            () = crate::cancelled(stop_rx.clone()) => break,
            state = states.recv() => {
                let Some(state) = state else { break; };
                if !core.lock().await.publish(state) { break; }
            }
        }
    }
    core.lock().await.revoke();
    stop.send_replace(true);
}

#[cfg(test)]
mod tests {
    use super::*;

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
                serde_json::from_str::<ScreenFrame>(&wire).unwrap(),
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
            serde_json::from_str::<ScreenFrame>(&late.frames.recv().await.unwrap())
                .unwrap()
                .seq,
            33
        );
        core.seq = u64::MAX - 1;
        assert!(core.publish(UiState::Muted));
        assert_eq!(
            serde_json::from_str::<ScreenFrame>(&healthy.frames.recv().await.unwrap())
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
}
