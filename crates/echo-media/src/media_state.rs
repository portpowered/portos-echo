// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host transport.rs; Modified by Port OS.
use crate::{Error, PcmFrame, PeerState, media_queue::Queue};
use std::sync::{Arc, Mutex};
use tokio::{sync::watch, task::JoinHandle};

pub(crate) struct Shared {
    pub(crate) state: watch::Sender<PeerState>,
    pub(crate) error: Mutex<Error>,
    pub(crate) send: Queue<PcmFrame>,
    pub(crate) recv: Queue<PcmFrame>,
    pub(crate) ingress: Arc<Queue<rtc::rtp::Packet>>,
    pub(crate) tasks: Mutex<Vec<JoinHandle<()>>>,
    readiness: Mutex<(bool, bool)>,
    #[cfg(test)]
    pub(crate) sent: watch::Sender<(u32, u32)>,
    #[cfg(test)]
    pub(crate) received: watch::Sender<(u32, u32)>,
}
impl Shared {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            state: watch::channel(PeerState::New).0,
            error: Mutex::new(Error::Transport),
            send: Queue::new(capacity),
            recv: Queue::new(capacity),
            ingress: Arc::new(Queue::new(capacity)),
            tasks: Mutex::new(Vec::new()),
            readiness: Mutex::new((false, false)),
            #[cfg(test)]
            sent: watch::channel((0, 0)).0,
            #[cfg(test)]
            received: watch::channel((0, 0)).0,
        }
    }
    pub(crate) fn task(&self, task: JoinHandle<()>) {
        let mut tasks = self.tasks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*self.state.borrow(), PeerState::Closed | PeerState::Failed) {
            task.abort();
        } else {
            tasks.push(task);
        }
    }
    pub(crate) fn connected(&self, transport: bool) {
        let mut ready = self.readiness.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if transport {
            ready.0 = true;
        } else {
            ready.1 = true;
        }
        if ready.0 && ready.1 {
            self.state.send_if_modified(|state| {
                if *state == PeerState::Connecting {
                    *state = PeerState::Connected;
                    true
                } else {
                    false
                }
            });
        }
    }
    pub(crate) fn fail(&self, error: Error) {
        self.state.send_if_modified(|state| {
            if matches!(state, PeerState::Closed | PeerState::Failed) {
                return false;
            }
            *self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = error;
            *state = PeerState::Failed;
            true
        });
        self.clear();
    }
    pub(crate) fn clear(&self) {
        self.send.close();
        self.recv.close();
        self.ingress.close();
    }
    pub(crate) fn check(&self) -> Result<(), Error> {
        match *self.state.borrow() {
            PeerState::Closed => Err(Error::Closed),
            PeerState::Failed => Err(*self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner)),
            _ => Ok(()),
        }
    }
}
