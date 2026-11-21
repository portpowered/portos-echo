use echo_contracts::endpoint::MAX_SNAPSHOT_BYTES as MAX_JPEG_BYTES;
use std::{
    sync::{Arc, Mutex},
    time::SystemTime,
};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

pub(crate) mod http;
mod wire;

pub struct Sample {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub sampled_at: SystemTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureError {
    Unavailable,
    Busy,
    PermissionDenied,
    Camera,
    Timeout,
}

type CaptureResult = Result<Sample, CaptureError>;

struct Pending {
    id: String,
    deadline: Instant,
    result: oneshot::Sender<CaptureResult>,
}

struct Session {
    nonce: String,
    generation: Arc<()>,
    events: mpsc::Sender<Event>,
    pending: Option<Pending>,
}

pub(super) enum Event {
    Capture {
        id: String,
        camera_id: String,
        deadline: Instant,
    },
    Cancel(String),
}

/// One native connection and at most one capture under its ownership.
#[derive(Clone, Default)]
pub struct Camera {
    session: Arc<Mutex<Option<Session>>>,
    upload: Arc<tokio::sync::Mutex<()>>,
}

impl Camera {
    #[must_use]
    pub fn connected(&self) -> bool {
        self.session
            .lock()
            .is_ok_and(|session| session.as_ref().is_some_and(|session| !session.events.is_closed()))
    }

    /// Request one capture. Dropping the future invalidates the result slot and
    /// notifies the owning stream so the app can close its camera resources.
    ///
    /// # Errors
    /// Reports a fixed camera failure, timeout, contention or missing app.
    pub async fn capture(&self, camera_id: &str, deadline: Instant) -> Result<Sample, CaptureError> {
        let (guard, result) = self.request(camera_id, deadline)?;
        let status = tokio::time::timeout_at(deadline, result).await;
        drop(guard);
        match status {
            Ok(Ok(result)) if Instant::now() < deadline => result,
            Ok(Err(_)) => Err(CaptureError::Unavailable),
            _ => Err(CaptureError::Timeout),
        }
    }

    fn request(
        &self,
        camera_id: &str,
        deadline: Instant,
    ) -> Result<(Capture, oneshot::Receiver<CaptureResult>), CaptureError> {
        let mut lock = self.session.lock().map_err(|_| CaptureError::Camera)?;
        if Instant::now() >= deadline {
            return Err(CaptureError::Timeout);
        }
        let session = lock.as_mut().ok_or(CaptureError::Unavailable)?;
        if session.pending.is_some() {
            return Err(CaptureError::Busy);
        }
        let id = hex::encode(rand::random::<[u8; 16]>());
        session
            .events
            .try_send(Event::Capture {
                id: id.clone(),
                camera_id: camera_id.into(),
                deadline,
            })
            .map_err(|_| CaptureError::Unavailable)?;
        let (result, received) = oneshot::channel();
        session.pending = Some(Pending {
            id: id.clone(),
            deadline,
            result,
        });
        Ok((
            Capture {
                owner: self.clone(),
                id,
            },
            received,
        ))
    }

    fn attach(&self, nonce: String) -> Option<(Connection, mpsc::Receiver<Event>)> {
        let mut lock = self.session.lock().ok()?;
        if lock.is_some() {
            return None;
        }
        let (events, receiver) = mpsc::channel(8);
        let generation = Arc::new(());
        *lock = Some(Session {
            nonce: nonce.clone(),
            generation: generation.clone(),
            events,
            pending: None,
        });
        Some((
            Connection {
                owner: self.clone(),
                nonce,
                generation,
            },
            receiver,
        ))
    }

    fn pending(&self, nonce: &str, id: &str) -> bool {
        self.session
            .lock()
            .is_ok_and(|lock| lock.as_ref().is_some_and(|session| session.owns(nonce, id)))
    }

    fn accept(&self, nonce: &str, id: &str, result: Result<Sample, CaptureError>) -> bool {
        let Ok(mut lock) = self.session.lock() else {
            return false;
        };
        lock.as_mut()
            .filter(|session| session.owns(nonce, id))
            .and_then(|session| session.pending.take())
            .is_some_and(|pending| pending.result.send(result).is_ok())
    }

    fn authorized_nonce(&self, nonce: &str) -> bool {
        self.session.lock().is_ok_and(|lock| {
            lock.as_ref()
                .is_some_and(|session| session.nonce == nonce && !session.events.is_closed())
        })
    }
}

impl Session {
    fn owns(&self, nonce: &str, id: &str) -> bool {
        self.nonce == nonce
            && !self.events.is_closed()
            && self.pending.as_ref().is_some_and(|pending| {
                pending.id == id && !pending.result.is_closed() && Instant::now() < pending.deadline
            })
    }
}

struct Connection {
    owner: Camera,
    nonce: String,
    generation: Arc<()>,
}

impl Connection {
    fn current(&self) -> bool {
        self.owner
            .session
            .lock()
            .is_ok_and(|lock| lock.as_ref().is_some_and(|session| self.owns(session)))
    }

    fn owns(&self, session: &Session) -> bool {
        Arc::ptr_eq(&self.generation, &session.generation)
    }

    fn live(&self, event: &Event) -> bool {
        match event {
            Event::Capture { id, .. } => self.owner.pending(&self.nonce, id),
            Event::Cancel(_) => self.current(),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut lock) = self.owner.session.lock()
            && lock.as_ref().is_some_and(|session| self.owns(session))
        {
            *lock = None;
        }
    }
}

struct Capture {
    owner: Camera,
    id: String,
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Ok(mut lock) = self.owner.session.lock()
            && let Some(session) = lock.as_mut()
            && session.pending.as_ref().is_some_and(|pending| pending.id == self.id)
        {
            session.pending = None;
            if session.events.try_send(Event::Cancel(self.id.clone())).is_err() {
                *lock = None;
            }
        }
    }
}
