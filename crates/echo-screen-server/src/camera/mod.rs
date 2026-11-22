use echo_contracts::endpoint::MAX_SNAPSHOT_BYTES as MAX_JPEG_BYTES;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tokio::{
    sync::{OwnedMutexGuard, mpsc, oneshot, watch},
    time::Instant,
};

pub(crate) mod http;
mod wire;

const STALE_UPLOAD_TIMEOUT: Duration = Duration::from_secs(3);

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
    // Dropping the pending capture ends any body read for this ID.
    retired: watch::Sender<()>,
    upload: Arc<tokio::sync::Mutex<()>>,
}

struct Upload {
    live: bool,
    deadline: Instant,
    retired: watch::Receiver<()>,
    _slot: OwnedMutexGuard<()>,
}

struct Session {
    nonce: String,
    generation: Arc<()>,
    events: mpsc::Sender<Event>,
    pending: Option<Pending>,
    retired: watch::Sender<()>,
    stale_upload: Arc<tokio::sync::Mutex<()>>,
}

pub(super) enum Event {
    Capture {
        id: String,
        camera_id: String,
        deadline: Instant,
    },
    Cancel(String),
}

/// Whether the native app holds the event stream, and the fixed reason its
/// latest attach was refused. Carries no nonce, key, address or capture ID.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Link {
    pub attached: bool,
    pub refused: Option<Refused>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refused {
    /// A browser-shaped or wrong-host request.
    Forbidden,
    /// A missing nonce or proof, a revoked pairing or a proof that does not verify.
    Unauthorized,
    /// Another stream already holds the camera.
    Conflict,
}

/// One native connection and at most one capture under its ownership.
#[derive(Clone)]
pub struct Camera {
    session: Arc<Mutex<Option<Session>>>,
    link: Arc<watch::Sender<Link>>,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            session: Arc::default(),
            link: Arc::new(watch::channel(Link::default()).0),
        }
    }
}

impl Camera {
    /// Observe attach, detach and refused attach attempts.
    #[must_use]
    pub fn link(&self) -> watch::Receiver<Link> {
        self.link.subscribe()
    }

    fn linked(&self, attached: bool) {
        let next = Link {
            attached,
            refused: None,
        };
        self.link.send_if_modified(|link| std::mem::replace(link, next) != next);
    }

    pub(crate) fn refused(&self, reason: Refused) {
        self.link
            .send_if_modified(|link| link.refused.replace(reason) != Some(reason));
    }

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
            retired: watch::channel(()).0,
            upload: Arc::default(),
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
            retired: watch::channel(()).0,
            stale_upload: Arc::default(),
        });
        self.linked(true);
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

    fn upload(&self, nonce: &str, id: &str) -> Option<Upload> {
        let lock = self.session.lock().ok()?;
        let session = lock.as_ref().filter(|session| session.attached(nonce))?;
        let pending = session.pending.as_ref().filter(|_| session.owns(nonce, id));
        // Stale results still require body authentication. Their reads cannot occupy a live slot.
        let (deadline, retired, slot) = match pending {
            Some(pending) => (pending.deadline, &pending.retired, &pending.upload),
            None => (
                Instant::now() + STALE_UPLOAD_TIMEOUT,
                &session.retired,
                &session.stale_upload,
            ),
        };
        Some(Upload {
            live: pending.is_some(),
            deadline,
            retired: retired.subscribe(),
            _slot: slot.clone().try_lock_owned().ok()?,
        })
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
        self.session
            .lock()
            .is_ok_and(|lock| lock.as_ref().is_some_and(|session| session.attached(nonce)))
    }
}

impl Session {
    fn attached(&self, nonce: &str) -> bool {
        self.nonce == nonce && !self.events.is_closed()
    }

    fn owns(&self, nonce: &str, id: &str) -> bool {
        self.attached(nonce)
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
            self.owner.linked(false);
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
                self.owner.linked(false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Camera, Link, Refused};

    #[test]
    fn link_follows_attach_refusal_and_detach() {
        let camera = Camera::default();
        let mut link = camera.link();
        assert_eq!(*link.borrow_and_update(), Link::default());
        camera.refused(Refused::Unauthorized);
        let unauthorized = Link {
            attached: false,
            refused: Some(Refused::Unauthorized),
        };
        assert_eq!(*link.borrow_and_update(), unauthorized);
        camera.refused(Refused::Unauthorized);
        assert!(!link.has_changed().unwrap(), "a repeated refusal is one state");

        let (connection, events) = camera.attach("first".into()).unwrap();
        let attached = Link {
            attached: true,
            refused: None,
        };
        assert_eq!(*link.borrow_and_update(), attached);
        assert!(camera.attach("second".into()).is_none());
        camera.refused(Refused::Conflict);
        let contended = Link {
            attached: true,
            refused: Some(Refused::Conflict),
        };
        assert_eq!(*link.borrow_and_update(), contended);

        drop(events);
        drop(connection);
        assert_eq!(*link.borrow_and_update(), Link::default());
        assert!(!camera.connected());
    }
}
