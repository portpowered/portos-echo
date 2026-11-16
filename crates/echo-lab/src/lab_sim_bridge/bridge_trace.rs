use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

pub(super) struct Trace {
    file: File,
    started: Instant,
}

impl Trace {
    pub(super) fn create(path: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| "bridge: cannot create fresh trace")?;
        Ok(Self {
            file,
            started: Instant::now(),
        })
    }

    pub(super) fn record(&mut self, event: &str, id: &str, count: Option<u64>) -> Result<(), String> {
        let mut value = json!({"event":event, "run_id":id, "elapsed_ms":self.started.elapsed().as_millis()});
        if let Some(count) = count {
            value["count"] = json!(count);
        }
        writeln!(self.file, "{value}")
            .and_then(|()| self.file.flush())
            .map_err(|_| "bridge: cannot append trace".into())
    }
}
