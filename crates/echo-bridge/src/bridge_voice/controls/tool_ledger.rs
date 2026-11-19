//! Persist reservations before effects; ambiguity never authorizes replay.
use echo_agent_api::ToolError;
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
};

pub(super) struct Ledger {
    file: File,
    keys: HashSet<String>,
    failed: bool,
}

impl Ledger {
    pub fn open(directory: &Path) -> Result<Self, ToolError> {
        crate::bridge_private::directory(directory).map_err(|_| ToolError::Transport)?;
        let path = directory.join("codex-tool-ledger.jsonl");
        if path.symlink_metadata().is_ok_and(|meta| !meta.file_type().is_file()) {
            return Err(ToolError::Transport);
        }
        let mut options = OpenOptions::new();
        let file = crate::bridge_private::file(options.create(true).read(true).append(true))
            .open(path)
            .map_err(|_| ToolError::Transport)?;
        file.try_lock().map_err(|_| ToolError::Transport)?;
        let mut keys = HashSet::new();
        for line in BufReader::new(&file).lines() {
            let key: String =
                serde_json::from_str(&line.map_err(|_| ToolError::Transport)?).map_err(|_| ToolError::Transport)?;
            if key.is_empty() || !keys.insert(key) {
                return Err(ToolError::Transport);
            }
        }
        Ok(Self {
            file,
            keys,
            failed: false,
        })
    }

    pub fn reserve(&mut self, key: &str) -> Result<(), ToolError> {
        if self.failed {
            return Err(ToolError::Transport);
        }
        if key.is_empty() || !self.keys.insert(key.into()) {
            return Err(ToolError::Denied);
        }
        let mut bytes = serde_json::to_vec(key).map_err(|_| ToolError::Transport)?;
        bytes.push(b'\n');
        // Mark failed before writing: partial output is never recoverable here.
        self.failed = true;
        self.file
            .write_all(&bytes)
            .and_then(|()| self.file.sync_data())
            .map_err(|_| ToolError::Transport)?;
        self.failed = false;
        Ok(())
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        // Release the owning description even during another thread's pre-exec fork.
        let _ = self.file.unlock();
    }
}
