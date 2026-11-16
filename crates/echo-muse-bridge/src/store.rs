use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Mutex,
};

use echo_agent_api::Task;
use echo_contracts::{ids::TaskId, task::TaskEvent};
use serde::{Deserialize, Serialize};

use crate::SideChat;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Live,
    Completed,
    Failed,
    Expired,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    #[serde(rename = "task_id")]
    pub id: TaskId,
    pub task: Task,
    #[serde(rename = "session_id")]
    pub session: SideChat,
    #[serde(rename = "deadline_unix_seconds")]
    pub deadline: u64,
    pub state: TaskState,
    pub events: VecDeque<TaskEvent>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    #[serde(rename = "tasks")]
    pub records: Vec<Record>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            version: 1,
            records: vec![],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store I/O failed")]
    Io(#[from] std::io::Error),
    #[error("invalid store JSON")]
    Json(#[from] serde_json::Error),
    #[error("unsupported or inconsistent store snapshot")]
    Invalid,
    #[error("store lock poisoned")]
    Poisoned,
}

/// One mutation owner; transactions publish only after durable commit succeeds.
/// Implementations must serialize transactions and discard changes on error.
pub trait TaskStore: Send + Sync {
    /// # Errors
    /// Returns the update error or a validation, lock, or durable commit error.
    fn transaction<R>(&self, update: impl FnOnce(&mut Snapshot) -> Result<R, StoreError>) -> Result<R, StoreError>;
}

#[derive(Default)]
pub struct InMemoryStore {
    snapshot: Mutex<Snapshot>,
}

impl TaskStore for InMemoryStore {
    fn transaction<R>(&self, update: impl FnOnce(&mut Snapshot) -> Result<R, StoreError>) -> Result<R, StoreError> {
        let mut current = self.snapshot.lock().map_err(|_| StoreError::Poisoned)?;
        let mut next = current.clone();
        let result = update(&mut next)?;
        validate(&next)?;
        *current = next;
        Ok(result)
    }
}

/// Sequential reopening is supported; independent concurrent writers are not.
/// The caller supplies a private directory outside the repository.
pub struct JsonFileStore {
    path: PathBuf,
    snapshot: Mutex<Snapshot>,
}

impl JsonFileStore {
    /// Open an existing snapshot, or initialize an empty store on first mutation.
    /// # Errors
    /// Fails closed on unreadable, invalid or unsupported existing snapshots.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let snapshot = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Snapshot::default(),
            Err(error) => return Err(error.into()),
        };
        validate(&snapshot)?;
        Ok(Self {
            path,
            snapshot: Mutex::new(snapshot),
        })
    }
}

impl TaskStore for JsonFileStore {
    fn transaction<R>(&self, update: impl FnOnce(&mut Snapshot) -> Result<R, StoreError>) -> Result<R, StoreError> {
        let mut current = self.snapshot.lock().map_err(|_| StoreError::Poisoned)?;
        let mut next = current.clone();
        let result = update(&mut next)?;
        validate(&next)?;
        if next != *current || !self.path.exists() {
            replace(&self.path, &next)?;
        }
        *current = next;
        Ok(result)
    }
}

fn replace(path: &Path, snapshot: &Snapshot) -> Result<(), StoreError> {
    let parent = path.parent().ok_or(StoreError::Invalid)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file().set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    serde_json::to_writer(file.as_file_mut(), snapshot)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| StoreError::Io(error.error))?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn validate(snapshot: &Snapshot) -> Result<(), StoreError> {
    if snapshot.version != 1 {
        return Err(StoreError::Invalid);
    }
    let mut ids = HashSet::new();
    let mut sessions = HashSet::new();
    for record in &snapshot.records {
        if !ids.insert(&record.id) || !sessions.insert(record.session.to_string()) {
            return Err(StoreError::Invalid);
        }
        validate_events(record)?;
    }
    Ok(())
}

fn validate_events(record: &Record) -> Result<(), StoreError> {
    let mut terminal = false;
    for event in &record.events {
        let (id, state) = match event {
            TaskEvent::Progress { task_id, .. } => (task_id, None),
            TaskEvent::Completed { task_id, .. } => (task_id, Some(TaskState::Completed)),
            TaskEvent::Failed {
                task_id,
                retryable,
                message,
            } => {
                if *retryable {
                    return Err(StoreError::Invalid);
                }
                let state = if record.state == TaskState::Expired && message == "Muse did not report a result in time" {
                    TaskState::Expired
                } else {
                    TaskState::Failed
                };
                (task_id, Some(state))
            }
        };
        if id != &record.id || terminal || state.is_some_and(|state| state != record.state) {
            return Err(StoreError::Invalid);
        }
        terminal = state.is_some();
    }
    if !record.events.is_empty()
        && (record.state == TaskState::Cancelled || (record.state != TaskState::Live && !terminal))
    {
        return Err(StoreError::Invalid);
    }
    Ok(())
}
