use std::{
    io::{self, BufRead, BufReader, Read},
    sync::OnceLock,
    time::Duration,
};

use echo_core_api::{Clock, SystemClock};

// A pipe belongs to exactly one child attempt. An oversized or malformed line
// is discarded up to its newline; it cannot supply a suffix interpreted as a receipt.
pub(super) fn drain(source: impl Read, ready: &OnceLock<Duration>) -> io::Result<()> {
    const LIMIT: usize = 4096;
    let mut reader = BufReader::new(source);
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(());
        }
        let newline = bytes.iter().position(|&byte| byte == b'\n');
        let consumed = newline.map_or(bytes.len(), |index| index + 1);
        if ready.get().is_none() && !oversized {
            oversized = line.len() + consumed > LIMIT;
            if !oversized {
                line.extend_from_slice(&bytes[..consumed]);
            }
        }
        if newline.is_some() {
            if !oversized
                && serde_json::from_slice::<serde_json::Value>(&line)
                    .is_ok_and(|value| value.get("event").and_then(serde_json::Value::as_str) == Some("bridge.ready"))
            {
                let _ = ready.set(SystemClock.now().monotonic);
            }
            line.clear();
            oversized = false;
        }
        reader.consume(consumed);
    }
}
