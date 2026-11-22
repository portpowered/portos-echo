use std::{
    collections::{HashSet, VecDeque},
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use echo_contracts::{ids::TaskId, task::TaskEvent};
use ring::rand::{SecureRandom, SystemRandom};
use rustix::{
    fs::{AtFlags, Mode, OFlags, fchmod, openat, renameat, unlinkat},
    io::Errno,
    process::geteuid,
};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;

use crate::{ServiceError, pairing};

const MAX_SNAPSHOT: u64 = 16 * 1024 * 1024;
const PRIVATE: Mode = Mode::RUSR.union(Mode::WUSR);

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    version: u32,
    pub(super) tasks: Vec<Record>,
}

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub(super) task_id: TaskId,
    pub(super) session_id: String,
    pub(super) deadline_unix_seconds: u64,
    pub(super) state: State,
    #[serde(deserialize_with = "strict_events")]
    pub(super) events: VecDeque<TaskEvent>,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
    Live,
    Completed,
    Failed,
    Expired,
    Cancelled,
}

pub(super) struct Store {
    directory: File,
    name: PathBuf,
    _lock: File,
    uncertain: bool,
}

impl Store {
    pub(super) fn open(path: &Path) -> Result<(Self, Snapshot), ServiceError> {
        let name = PathBuf::from(path.file_name().ok_or(ServiceError::Storage)?);
        let directory =
            pairing::open_directory(path.parent().ok_or(ServiceError::Storage)?).map_err(|_| ServiceError::Storage)?;
        let metadata = directory.metadata().map_err(|_| ServiceError::Storage)?;
        if metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
            return Err(ServiceError::Storage);
        }
        let flags = pairing::private_flags() | OFlags::RDWR | OFlags::CREATE;
        let lock = openat(&directory, name.with_added_extension("lock"), flags, PRIVATE)
            .map(File::from)
            .map_err(|_| ServiceError::Storage)?;
        check_file(&lock)?;
        lock.try_lock().map_err(|_| ServiceError::Storage)?;
        let store = Self {
            directory,
            name,
            _lock: lock,
            uncertain: false,
        };
        let snapshot = store.load()?;
        Ok((store, snapshot))
    }

    fn load(&self) -> Result<Snapshot, ServiceError> {
        let flags = pairing::private_flags() | OFlags::RDONLY;
        let file = match openat(&self.directory, &self.name, flags, Mode::empty()) {
            Ok(file) => File::from(file),
            Err(Errno::NOENT) => {
                return Ok(Snapshot {
                    version: 1,
                    tasks: Vec::new(),
                });
            }
            Err(_) => return Err(ServiceError::Storage),
        };
        check_file(&file)?;
        let mut bytes = Vec::new();
        file.take(MAX_SNAPSHOT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ServiceError::Storage)?;
        if bytes.len() as u64 > MAX_SNAPSHOT {
            return Err(ServiceError::Storage);
        }
        let snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(|_| ServiceError::Storage)?;
        snapshot.valid().then_some(snapshot).ok_or(ServiceError::Storage)
    }

    pub(super) fn ready(&self) -> Result<(), ServiceError> {
        if self.uncertain {
            Err(ServiceError::Storage)
        } else {
            Ok(())
        }
    }

    pub(super) fn save(&mut self, snapshot: &Snapshot) -> Result<(), ServiceError> {
        self.ready()?;
        let bytes = serde_json::to_vec(snapshot).map_err(|_| ServiceError::Storage)?;
        if bytes.len() as u64 > MAX_SNAPSHOT {
            return Err(ServiceError::Storage);
        }
        let mut suffix = [0; 16];
        SystemRandom::new()
            .fill(&mut suffix)
            .map_err(|_| ServiceError::Storage)?;
        let staging = format!(".muse-tasks-{}", hex::encode(suffix));
        let flags = pairing::private_flags() | OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL;
        let mut file =
            File::from(openat(&self.directory, &staging, flags, PRIVATE).map_err(|_| ServiceError::Storage)?);
        if self.replace(&staging, &mut file, &bytes).is_err() {
            let _ = unlinkat(&self.directory, staging, AtFlags::empty());
            return Err(ServiceError::Storage);
        }
        // The new inode is visible after rename, so an uncertain sync must stop
        // further transactions until a fresh open can read the selected snapshot.
        self.uncertain = self.directory.sync_all().is_err();
        self.ready()
    }

    fn replace(&self, staging: &str, file: &mut File, bytes: &[u8]) -> Result<(), ServiceError> {
        fchmod(&*file, PRIVATE).map_err(|_| ServiceError::Storage)?;
        file.write_all(bytes).map_err(|_| ServiceError::Storage)?;
        file.sync_all().map_err(|_| ServiceError::Storage)?;
        renameat(&self.directory, staging, &self.directory, &self.name).map_err(|_| ServiceError::Storage)
    }
}

fn check_file(file: &File) -> Result<(), ServiceError> {
    let metadata = file.metadata().map_err(|_| ServiceError::Storage)?;
    if metadata.is_file()
        && metadata.mode() & 0o7777 == 0o600
        && metadata.uid() == geteuid().as_raw()
        && metadata.nlink() == 1
        && metadata.len() <= MAX_SNAPSHOT
    {
        Ok(())
    } else {
        Err(ServiceError::Storage)
    }
}

impl Snapshot {
    fn valid(&self) -> bool {
        let mut ids = HashSet::new();
        let mut sessions = HashSet::new();
        self.version == 1
            && self
                .tasks
                .iter()
                .all(|record| ids.insert(&record.task_id) && sessions.insert(&record.session_id) && record.valid())
    }
}

impl Record {
    fn valid(&self) -> bool {
        if !valid_session(&self.session_id) || (self.state == State::Cancelled && !self.events.is_empty()) {
            return false;
        }
        let mut events = self.events.iter().peekable();
        while let Some(event) = events.next() {
            let last = events.peek().is_none();
            let valid = match event {
                TaskEvent::Progress { task_id, .. } => task_id == &self.task_id,
                TaskEvent::Completed { task_id, .. } => {
                    last && self.state == State::Completed && task_id == &self.task_id
                }
                TaskEvent::Failed { task_id, retryable, .. } => {
                    last && !retryable
                        && matches!(self.state, State::Failed | State::Expired)
                        && task_id == &self.task_id
                }
            };
            if !valid {
                return false;
            }
        }
        // A consumed terminal event leaves an empty tombstone. Pending progress
        // cannot outlive the terminal event that followed it in the queue.
        self.state == State::Live || !matches!(self.events.back(), Some(TaskEvent::Progress { .. }))
    }
}

fn valid_session(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn strict_events<'de, D: Deserializer<'de>>(deserializer: D) -> Result<VecDeque<TaskEvent>, D::Error> {
    VecDeque::<Value>::deserialize(deserializer)?
        .into_iter()
        .map(|value| {
            let event: TaskEvent = serde_json::from_value(value.clone()).map_err(D::Error::custom)?;
            let fields = if matches!(event, TaskEvent::Failed { .. }) {
                4
            } else {
                3
            };
            if value.as_object().is_some_and(|object| object.len() == fields) {
                Ok(event)
            } else {
                Err(D::Error::custom("unexpected task event fields"))
            }
        })
        .collect()
}
