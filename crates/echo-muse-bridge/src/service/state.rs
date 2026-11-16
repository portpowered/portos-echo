use std::{
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use super::config::{ClockConfig, private_path};
use crate::{CliGadget, Clock, EpochClock, JsonFileStore, MuseBackend, Snapshot, StoreError, TaskStore};
use echo_contracts::secret::Secret;
use tokio::sync::{Notify, Semaphore};

pub(super) const STORE_BYTES: u64 = 16_777_216;
pub(super) const STORE_RECORDS: usize = 4096;

pub(super) struct StoreInner {
    file: JsonFileStore,
    failed: AtomicBool,
    // Never unlink the inode; hold the OS lease through all child cleanup.
    _lease: File,
}

#[derive(Clone)]
pub(super) struct ServiceStore(Arc<StoreInner>);

impl ServiceStore {
    pub fn open(path: &Path) -> Result<Self, &'static str> {
        let parent = path.parent().ok_or("invalid_store")?;
        private_path(parent, true)?;
        let parent = parent.canonicalize().map_err(|_| "invalid_store")?;
        let path = parent.join(path.file_name().ok_or("invalid_store")?);
        let lock_path = path.with_added_extension("lock");
        if std::fs::symlink_metadata(&lock_path).is_ok() {
            private_path(&lock_path, false)?;
        }
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)
            .map_err(|_| "store_unavailable")?;
        lease.try_lock().map_err(|_| "store_already_owned")?;
        if path.exists() {
            private_path(&path, false)?;
            if std::fs::metadata(&path).map_err(|_| "store_unavailable")?.len() > STORE_BYTES {
                return Err("store_capacity");
            }
        } else if std::fs::symlink_metadata(&path).is_ok() {
            return Err("unsafe_store");
        }
        let store = Self(Arc::new(StoreInner {
            file: JsonFileStore::open(path).map_err(|_| "store_unavailable")?,
            failed: AtomicBool::new(false),
            _lease: lease,
        }));
        store.transaction(|_| Ok(())).map_err(|_| "store_unavailable")?;
        Ok(store)
    }

    pub fn healthy(&self) -> bool {
        !self.0.failed.load(Ordering::SeqCst)
    }

    pub fn contains(&self, id: &echo_contracts::ids::TaskId) -> Result<bool, StoreError> {
        self.transaction(|snapshot| Ok(snapshot.records.iter().any(|record| &record.id == id)))
    }
}

impl TaskStore for ServiceStore {
    fn transaction<R>(&self, update: impl FnOnce(&mut Snapshot) -> Result<R, StoreError>) -> Result<R, StoreError> {
        if !self.healthy() {
            return Err(StoreError::Invalid);
        }
        let result = self.0.file.transaction(|snapshot| {
            let result = update(snapshot)?;
            quota(snapshot)?;
            Ok(result)
        });
        if result.is_err() {
            self.0.failed.store(true, Ordering::SeqCst);
        }
        result
    }
}

pub(super) fn quota(snapshot: &Snapshot) -> Result<(), StoreError> {
    if snapshot.records.len() > STORE_RECORDS || serde_json::to_vec(snapshot)?.len() as u64 > STORE_BYTES {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

#[derive(Clone)]
pub(super) struct ServiceClock {
    pub now: Option<Arc<AtomicU64>>,
    pub changed: Arc<Notify>,
}

impl ServiceClock {
    pub fn new(config: &ClockConfig) -> Self {
        let now = match config {
            ClockConfig::System => None,
            ClockConfig::ControlledStdin { initial_unix_seconds } => {
                Some(Arc::new(AtomicU64::new(*initial_unix_seconds)))
            }
        };
        Self {
            now,
            changed: Arc::new(Notify::new()),
        }
    }
    pub fn set(&self, value: u64) -> Result<(), &'static str> {
        let now = self.now.as_ref().ok_or("clock_control_unavailable")?;
        now.try_update(Ordering::SeqCst, Ordering::SeqCst, |previous| {
            (value >= previous).then_some(value)
        })
        .map_err(|_| "clock_moved_backwards")?;
        self.changed.notify_waiters();
        Ok(())
    }
}

impl Clock for ServiceClock {
    fn now(&self) -> u64 {
        self.now
            .as_ref()
            .map_or_else(|| EpochClock.now(), |now| now.load(Ordering::SeqCst))
    }
    async fn wait_until(&self, deadline: u64) {
        if self.now.is_none() {
            return EpochClock.wait_until(deadline).await;
        }
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.now() >= deadline {
                return;
            }
            changed.await;
        }
    }
}

pub(super) struct Service {
    pub jobs: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    pub clock: ServiceClock,
    pub backend: MuseBackend<CliGadget, ServiceClock, ServiceStore>,
    pub store: ServiceStore,
    pub credential: Secret<Vec<u8>>,
    pub children: Semaphore,
    pub stopping: AtomicBool,
}
