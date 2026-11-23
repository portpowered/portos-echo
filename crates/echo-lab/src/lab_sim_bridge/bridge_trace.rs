use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use echo_bridge::world_witness::HelperWorld;

pub(super) struct Trace<'a> {
    file: File,
    started: Instant,
    world: &'a HelperWorld,
}

impl<'a> Trace<'a> {
    #[allow(clippy::disallowed_methods, reason = "Existing host time")]
    pub(super) fn create(path: &Path, world: &'a HelperWorld) -> Result<Self, String> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| {
                world.trace_failed();
                "bridge: cannot create fresh trace"
            })?;
        Ok(Self {
            file,
            started: Instant::now(),
            world,
        })
    }

    pub(super) fn record(&mut self, event: &str, id: &str, count: Option<u64>) -> Result<(), String> {
        let mut value = json!({"event":event, "run_id":id, "elapsed_ms":self.started.elapsed().as_millis()});
        if let Some(count) = count {
            value["count"] = json!(count);
        }
        writeln!(self.file, "{value}")
            .and_then(|()| self.file.flush())
            .map_err(|_| {
                self.world.trace_failed();
                "bridge: cannot append trace".into()
            })
    }
}
