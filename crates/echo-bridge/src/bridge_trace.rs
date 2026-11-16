//! Closed event vocabulary and counters only. Never serialize provider events.
use crate::Error;
use serde_json::{Map, Value};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    time::Instant,
};

pub struct Trace {
    writer: File,
    started: Instant,
    mode: &'static str,
}

impl Trace {
    /// # Errors
    /// Returns a safe I/O category; paths never enter diagnostics.
    pub fn open(path: &Path, mode: &'static str) -> Result<Self, Error> {
        let writer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|_| Error::Trace)?;
        Ok(Self {
            writer,
            started: Instant::now(),
            mode,
        })
    }

    /// Callers supply only authored event names and safe, bounded fields.
    /// # Errors
    /// A serialization/write/flush failure prevents claiming completion.
    pub fn record(&mut self, event: &str, run: Option<&str>, fields: Value) -> Result<(), Error> {
        let mut entry = Map::new();
        entry.insert("event".into(), event.into());
        entry.insert(
            "elapsed_ms".into(),
            u64::try_from(self.started.elapsed().as_millis())
                .unwrap_or(u64::MAX)
                .into(),
        );
        entry.insert("provider_auth_mode".into(), self.mode.into());
        if let Some(run) = run {
            entry.insert("run_id".into(), run.into());
        }
        if let Value::Object(fields) = fields {
            entry.extend(fields);
        }
        let bytes = serde_json::to_vec(&entry).map_err(|_| Error::Trace)?;
        if bytes.len() > 4096 {
            return Err(Error::Trace);
        }
        self.writer
            .write_all(&bytes)
            .and_then(|()| self.writer.write_all(b"\n"))
            .and_then(|()| self.writer.flush())
            .map_err(|_| Error::Trace)
    }
}
