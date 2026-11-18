// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host transport.rs; Modified by Port OS.
use crate::{Error, PcmFrame, PeerState, SetupObserver, SetupStage, media_queue::Queue};
use std::sync::{Arc, Mutex};
use tokio::{sync::watch, task::JoinHandle};

pub(crate) struct Shared {
    pub(crate) state: watch::Sender<PeerState>,
    pub(crate) error: Mutex<Error>,
    pub(crate) send: Queue<PcmFrame>,
    pub(crate) recv: Queue<PcmFrame>,
    pub(crate) ingress: Arc<Queue<rtc::rtp::Packet>>,
    pub(crate) tasks: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) resets: Queue<tokio::sync::oneshot::Sender<Result<(), Error>>>,
    readiness: Mutex<(bool, bool)>,
    setup: Mutex<(Option<SetupObserver>, u8)>,
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
            resets: Queue::new(1),
            readiness: Mutex::new((false, false)),
            setup: Mutex::new((None, 0)),
            #[cfg(test)]
            sent: watch::channel((0, 0)).0,
            #[cfg(test)]
            received: watch::channel((0, 0)).0,
        }
    }
    pub(crate) fn observe_setup(&self, observer: SetupObserver) -> Result<(), Error> {
        let mut setup = self.setup.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *self.state.borrow() != PeerState::New || setup.0.is_some() {
            return Err(Error::State);
        }
        setup.0 = Some(observer);
        Ok(())
    }
    pub(crate) fn begin(&self) -> Result<(), Error> {
        let _setup = self.setup.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.check()?;
        let changed = self.state.send_if_modified(|state| {
            if *state == PeerState::New {
                *state = PeerState::Connecting;
                true
            } else {
                false
            }
        });
        if changed { Ok(()) } else { Err(Error::State) }
    }
    pub(crate) fn setup_stage(&self, stage: SetupStage) {
        let mut setup = self.setup.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*self.state.borrow(), PeerState::Closed | PeerState::Failed) {
            return;
        }
        let bit = match stage {
            SetupStage::IceConnected => 1,
            SetupStage::DataChannelOpen => 2,
        };
        if setup.1 & bit != 0 {
            return;
        }
        setup.1 |= bit;
        if let Some(observer) = &setup.0
            && observer(stage).is_err()
        {
            self.fail(Error::Transport);
        }
    }
    pub(crate) fn close(&self) {
        // Wait for any in-flight callback before making close terminal.
        let mut setup = self.setup.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        setup.0 = None;
        self.state.send_replace(PeerState::Closed);
        self.clear();
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
        self.resets.close();
    }
    pub(crate) fn check(&self) -> Result<(), Error> {
        match *self.state.borrow() {
            PeerState::Closed => Err(Error::Closed),
            PeerState::Failed => Err(*self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner)),
            _ => Ok(()),
        }
    }
}

impl Shared {
    pub(crate) async fn receive(&self, mut decoder: crate::media_audio::Decoder) {
        loop {
            tokio::select! {
                biased;
                reset = self.resets.pop() => {
                    let Some(ack) = reset else { break };
                    let result = decoder.reset();
                    // The receive task is the sole decoder/PCM producer. These
                    // discards and acknowledgement have no intervening await.
                    self.ingress.discard_with(|| {
                        self.recv.discard();
                        let _ = ack.send(result);
                    });
                    if let Err(error) = result { self.fail(error); break; }
                }
                packet = self.ingress.pop() => {
                    let Some(packet) = packet else { break };
                    let expansion = decoder.decode(&packet);
                    #[cfg(test)]
                    let emitted = expansion.as_ref().is_ok_and(Option::is_some);
                    match expansion {
                        Ok(Some(frame)) => self.recv.push(frame),
                        Ok(None) => {}
                        Err(error) => { self.fail(error); break; }
                    }
                    #[cfg(test)]
                    self.received.send_modify(|p| { p.0 += 1; p.1 += u32::from(emitted); });
                }
            }
        }
    }
}
