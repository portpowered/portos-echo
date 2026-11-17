//! Invocation-owned JSONL cursor. EOF yields; only newly appended bytes are parsed.
use super::*;
use std::{collections::VecDeque, io};
use tokio::io::AsyncRead;

#[derive(Default)]
struct Trace {
    partial: Vec<u8>,
    tail: VecDeque<String>,
    records: usize,
    bytes: usize,
}

impl Trace {
    fn consume(&mut self, bytes: &[u8], expected: &str) -> bool {
        self.bytes += bytes.len();
        self.partial.extend_from_slice(bytes);
        let mut matched = false;
        let mut consumed = 0;
        for (end, _) in self.partial.iter().enumerate().filter(|(_, byte)| **byte == b'\n') {
            let value = serde_json::from_slice::<Value>(&self.partial[consumed..end]);
            matched |= value.as_ref().is_ok_and(|row| row["event"] == expected);
            self.tail.push_back(summary(value.as_ref().ok()));
            if self.tail.len() > 8 {
                self.tail.pop_front();
            }
            self.records += 1;
            consumed = end + 1;
        }
        self.partial.drain(..consumed);
        matched
    }

    fn failure(&self, expected: &str, reason: &str, state: &str) -> String {
        format!(
            "trace wait for {}: {reason}; {state}; bytes={} records={} partial_bytes={}\nsanitized trace tail: {:?}",
            safe_event(expected),
            self.bytes,
            self.records,
            self.partial.len(),
            self.tail
        )
    }
}

// Only fixed event vocabulary and unsigned timing leave the trace. Never print arbitrary
// fields, malformed records, partial bytes, paths, or error messages containing payloads.
fn safe_event(event: &str) -> &str {
    match event {
        "auth.relink_required"
        | "run.failed"
        | "run.refused"
        | "mic.frames"
        | "reply.started"
        | "reply.played"
        | "session.commentary.append"
        | "live.closed"
        | "kill.enabled"
        | "kill.cleared"
        | "bridge.started"
        | "screen.listening"
        | "wyoming.connected"
        | "wyoming.run_started"
        | "bridge.error" => event,
        _ => "[redacted event]",
    }
}

fn summary(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "[invalid JSON record]".into();
    };
    format!(
        "{} elapsed_ms={:?}",
        safe_event(value["event"].as_str().unwrap_or("")),
        value["elapsed_ms"].as_u64()
    )
}

pub(super) fn child_state(result: io::Result<Option<std::process::ExitStatus>>) -> String {
    match result {
        Ok(None) => "bridge try_wait=alive".into(),
        Ok(Some(status)) => format!("bridge try_wait=exited ({status})"),
        Err(error) => format!("bridge try_wait=unknown ({:?})", error.kind()),
    }
}

async fn read_events(reader: &mut (impl AsyncRead + Unpin), trace: &mut Trace, event: &str) -> io::Result<()> {
    let mut bytes = [0; 8192];
    loop {
        let count = reader.read(&mut bytes).await?;
        if count == 0 {
            tokio::time::sleep(POLL_INTERVAL).await;
        } else if trace.consume(&bytes[..count], event) {
            return Ok(());
        }
    }
}

async fn follow(path: &Path, trace: &mut Trace, event: &str) -> io::Result<()> {
    let mut file = loop {
        match tokio::fs::File::open(path).await {
            Ok(file) => break file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => tokio::time::sleep(POLL_INTERVAL).await,
            Err(error) => return Err(error),
        }
    };
    read_events(&mut file, trace, event).await
}

pub(crate) async fn observe(path: &Path, event: &str, mut state: impl FnMut() -> String) {
    let mut trace = Trace::default();
    let result = tokio::time::timeout(OBSERVATION_TIMEOUT, follow(path, &mut trace, event)).await;
    let reason = match result {
        Ok(Ok(())) => return,
        Ok(Err(ref error)) => format!("read failed ({:?})", error.kind()),
        Err(_) => format!("timed out after {OBSERVATION_TIMEOUT:?}"),
    };
    assert!(
        matches!(result, Ok(Ok(()))),
        "{}",
        trace.failure(event, &reason, &state())
    );
}

#[path = "trace_wait_tests.rs"]
mod tests;
