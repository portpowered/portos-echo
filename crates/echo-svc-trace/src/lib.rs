//! Generation-scoped JSONL writer. A single worker owns the append handle.
use echo_core_api::{TraceDrain, TraceFailure, TraceRow};
use serde_json::Map;
use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
};

struct Running {
    signal: SyncSender<()>,
    thread: JoinHandle<()>,
}

pub struct Writer(Mutex<Option<Running>>);

impl Writer {
    /// # Errors
    /// Returns a fixed category if opening the file or starting the worker fails.
    pub fn open(path: &Path, source: Arc<impl TraceDrain + 'static>) -> Result<Self, TraceFailure> {
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        Self::start(options.open(path).map_err(|_| TraceFailure)?, source)
    }

    fn start(sink: impl Write + Send + 'static, source: Arc<impl TraceDrain + 'static>) -> Result<Self, TraceFailure> {
        let (signal, receiver) = sync_channel(1);
        let thread = thread::Builder::new()
            .name("echo-trace".into())
            .spawn(move || run(sink, &*source, &receiver))
            .map_err(|_| TraceFailure)?;
        Ok(Self(Mutex::new(Some(Running { signal, thread }))))
    }

    pub fn wake(&self) {
        if let Ok(running) = self.0.lock()
            && let Some(running) = running.as_ref()
        {
            // One pending wake is sufficient, even if the sink is stalled.
            let _ = running.signal.try_send(());
        }
    }

    /// Close admission at the source first. The worker drains accepted rows before joining.
    /// # Errors
    /// Reports a worker that could not be joined.
    pub fn finish(&self) -> Result<(), TraceFailure> {
        let running = self.0.lock().map_err(|_| TraceFailure)?.take();
        if let Some(Running { signal, thread }) = running {
            drop(signal);
            thread.join().map_err(|_| TraceFailure)?;
        }
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn run(mut sink: impl Write, source: &impl TraceDrain, receiver: &Receiver<()>) {
    loop {
        let signalled = receiver.recv().is_ok();
        while let Some(pending) = source.next_row() {
            let result = write_row(&mut sink, pending.row);
            if result.is_err() {
                source.write_failed();
            }
            if let Some(receipt) = pending.receipt {
                let _ = receipt.send(result);
            }
        }
        if !signalled {
            break;
        }
    }
}

fn write_row(sink: &mut impl Write, row: TraceRow) -> Result<(), TraceFailure> {
    let mut entry = Map::new();
    entry.insert("event".into(), row.event.into());
    entry.insert("elapsed_ms".into(), row.elapsed_ms.into());
    entry.insert("provider_auth_mode".into(), row.mode.into());
    if let Some(run) = row.run {
        entry.insert("run_id".into(), run.into());
    }
    entry.extend(row.fields);
    let mut bytes = Bytes::new(row.limit);
    serde_json::to_writer(&mut bytes, &entry).map_err(|_| TraceFailure)?;
    if !row.payload_fields.is_empty() {
        for key in row.payload_fields {
            entry.remove(*key);
        }
        serde_json::to_writer(&mut Bytes::new(4096), &entry).map_err(|_| TraceFailure)?;
    }
    bytes.data.push(b'\n');
    sink.write_all(&bytes.data)
        .and_then(|()| sink.flush())
        .map_err(|_| TraceFailure)
}

struct Bytes {
    data: Vec<u8>,
    limit: usize,
}

impl Bytes {
    fn new(limit: usize) -> Self {
        Self {
            data: Vec::new(),
            limit,
        }
    }
}

impl Write for Bytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.data.len() {
            return Err(io::Error::other("trace row limit"));
        }
        self.data.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
