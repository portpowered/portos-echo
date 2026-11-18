//! Receipts exist only inside the guarded clock owner's task-local scope.
use crate::Error;
use echo_screen_contract::UiState;
use echo_screen_server::ScreenServer;
use serde_json::{Value, json};
use std::{
    future::Future,
    sync::{Arc, Mutex, Weak},
    task::Poll,
    time::Duration,
};
use tokio::{
    sync::{Notify, mpsc, oneshot},
    time::Instant,
};

tokio::task_local! {
    pub(super) static CURRENT: Context;
}

type Request = oneshot::Sender<()>;

#[derive(Clone)]
pub(crate) struct Context(Arc<Shared>);

struct Shared {
    start: Instant,
    state: Mutex<State>,
    changed: Notify,
}

#[derive(Default)]
struct State {
    submitted: u64,
    written: u64,
    failed: bool,
    pcm: Vec<Value>,
    states: Vec<Value>,
    voice: Weak<mpsc::UnboundedSender<Request>>,
    publisher: Weak<mpsc::UnboundedSender<Request>>,
    publisher_registered: bool,
    screen: Weak<ScreenServer>,
}

pub(crate) struct VoiceDrain {
    _keep: Arc<mpsc::UnboundedSender<Request>>,
    receive: mpsc::UnboundedReceiver<Request>,
}

impl VoiceDrain {
    /// A successful terminal owner has no work left to drain. A dropped or
    /// cancelled owner deliberately leaves its pending requests unacknowledged.
    pub(crate) fn finish(mut self) {
        self.receive.close();
        while let Ok(request) = self.receive.try_recv() {
            let _ = request.send(());
        }
    }

    /// Polled only after due timers and ready voice work, never before them.
    pub(crate) async fn acknowledge(&mut self) {
        if let Some(request) = self.receive.recv().await {
            let _ = request.send(());
        }
    }
}

impl Context {
    pub(super) fn new() -> Self {
        Self(Arc::new(Shared {
            start: Instant::now(),
            state: Mutex::default(),
            changed: Notify::new(),
        }))
    }

    pub(crate) fn current() -> Option<Self> {
        CURRENT.try_with(Clone::clone).ok()
    }

    pub(crate) fn voice(&self) -> Result<VoiceDrain, Error> {
        let (send, receive) = mpsc::unbounded_channel();
        let send = Arc::new(send);
        let mut state = self.0.state.lock().map_err(|_| Error::Control)?;
        if state.voice.upgrade().is_some() {
            return Err(Error::Control);
        }
        state.voice = Arc::downgrade(&send);
        Ok(VoiceDrain { _keep: send, receive })
    }

    pub(crate) fn screen(&self, server: &Arc<ScreenServer>) -> Result<(), Error> {
        self.0.state.lock().map_err(|_| Error::Control)?.screen = Arc::downgrade(server);
        Ok(())
    }

    pub(crate) fn publisher(&self) -> Result<VoiceDrain, Error> {
        let (send, receive) = mpsc::unbounded_channel();
        let send = Arc::new(send);
        let mut state = self.0.state.lock().map_err(|_| Error::Control)?;
        if state.publisher.upgrade().is_some() {
            return Err(Error::Control);
        }
        state.publisher = Arc::downgrade(&send);
        state.publisher_registered = true;
        Ok(VoiceDrain { _keep: send, receive })
    }

    pub(crate) fn submitted(&self) -> Result<(), Error> {
        let mut state = self.0.state.lock().map_err(|_| Error::Control)?;
        state.submitted = state.submitted.checked_add(1).ok_or(Error::Control)?;
        Ok(())
    }

    pub(crate) fn projected(&self, projected: &UiState) -> Result<(), Error> {
        let mut state = self.0.state.lock().map_err(|_| Error::Control)?;
        if state.states.len() >= 8192 {
            return Err(Error::Control);
        }
        state.states.push(json!({"epoch_ms":self.epoch()?, "state":projected}));
        Ok(())
    }

    pub(super) fn wrote(&self, frame: &[u8]) -> Result<(), Error> {
        let samples = frame.as_chunks::<2>().0;
        let nonzero = samples
            .iter()
            .filter(|sample| i16::from_le_bytes(**sample) != 0)
            .count();
        let audible = samples
            .iter()
            .filter(|sample| i16::from_le_bytes(**sample).unsigned_abs() > 64)
            .count();
        let mut state = self.0.state.lock().map_err(|_| Error::Control)?;
        if state.pcm.len() >= 8192 {
            return Err(Error::Control);
        }
        state.written = state.written.checked_add(1).ok_or(Error::Control)?;
        state.pcm.push(json!({"epoch_ms":self.epoch()?, "pcm_bytes":frame.len(),
            "nonzero_samples":nonzero,"audible_samples":audible}));
        self.0.changed.notify_one();
        Ok(())
    }

    pub(crate) fn failed(&self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.failed = true;
        }
        self.0.changed.notify_one();
    }

    fn epoch(&self) -> Result<u64, Error> {
        Instant::now()
            .duration_since(self.0.start)
            .as_millis()
            .try_into()
            .map_err(|_| Error::Control)
    }

    pub(super) async fn drain(&self) -> Result<(), Error> {
        loop {
            self.drain_pass().await?;
            let before = {
                let state = self.0.state.lock().map_err(|_| Error::Control)?;
                (state.submitted, state.states.len())
            };
            let voice = self.0.state.lock().map_err(|_| Error::Control)?.voice.upgrade();
            if let Some(voice) = voice {
                let (send, receive) = oneshot::channel();
                voice.send(send).map_err(|_| Error::Control)?;
                receive.await.map_err(|_| Error::Control)?;
            }
            let state = self.0.state.lock().map_err(|_| Error::Control)?;
            if state.failed || state.written > state.submitted {
                return Err(Error::Control);
            }
            if before == (state.submitted, state.states.len()) && state.written == state.submitted {
                return Ok(());
            }
        }
    }

    async fn drain_pass(&self) -> Result<(), Error> {
        let voice = self.0.state.lock().map_err(|_| Error::Control)?.voice.upgrade();
        if let Some(voice) = voice {
            let (send, receive) = oneshot::channel();
            voice.send(send).map_err(|_| Error::Control)?;
            receive.await.map_err(|_| Error::Control)?;
        }
        let publisher = {
            let state = self.0.state.lock().map_err(|_| Error::Control)?;
            let publisher = state.publisher.upgrade();
            if state.publisher_registered && publisher.is_none() {
                return Err(Error::Control);
            }
            publisher
        };
        if let Some(publisher) = publisher {
            let (send, receive) = oneshot::channel();
            publisher.send(send).map_err(|_| Error::Control)?;
            receive.await.map_err(|_| Error::Control)?;
        }
        loop {
            let changed = self.0.changed.notified();
            {
                let state = self.0.state.lock().map_err(|_| Error::Control)?;
                if state.failed || state.written > state.submitted {
                    return Err(Error::Control);
                }
                if state.written == state.submitted {
                    break;
                }
            }
            changed.await;
        }
        let screen = self
            .0
            .state
            .lock()
            .map_err(|_| Error::Control)?
            .screen
            .upgrade()
            .ok_or(Error::Control)?;
        screen
            .flush(Instant::now() + Duration::from_secs(5))
            .await
            .map_err(|_| Error::Control)
    }

    pub(super) async fn advance(&self, milliseconds: u64) -> Result<Value, Error> {
        self.drain().await?;
        let duration = Duration::from_millis(milliseconds);
        Instant::now().checked_add(duration).ok_or(Error::Control)?;
        tokio::time::advance(duration).await;
        self.drain().await?;
        let mut state = self.0.state.lock().map_err(|_| Error::Control)?;
        Ok(json!({"event":"bridge.clock_advanced", "milliseconds":milliseconds,
            "epoch_ms":self.epoch()?, "pcm":std::mem::take(&mut state.pcm),
            "states":std::mem::take(&mut state.states)}))
    }
}

/// Paused Tokio deadlines cannot bound a missing clock-step acknowledgement.
/// This watchdog never changes logical time and never converts timeout to success.
pub(super) async fn bounded<T>(future: impl Future<Output = Result<T, Error>>, limit: Duration) -> Result<T, Error> {
    let start = std::time::Instant::now();
    tokio::pin!(future);
    std::future::poll_fn(|cx| {
        if start.elapsed() >= limit {
            return Poll::Ready(Err(Error::Control));
        }
        let result = future.as_mut().poll(cx);
        if result.is_pending() {
            cx.waker().wake_by_ref();
        }
        result
    })
    .await
}
