//! Private bounded trace reader. Raw observer payloads never enter report events.
use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Default)]
pub struct Trace {
    pub records: Vec<Value>,
    pending: Vec<u8>,
    timestamps: BTreeMap<String, u64>,
    bytes: usize,
    invalid: bool,
}

impl Trace {
    // Elapsed times and run IDs belong to one collected production process.
    // Keep its validated records and total byte budget for final oracles.
    pub fn restarted(&mut self) -> Result<(), String> {
        self.finish()?;
        self.timestamps.clear();
        Ok(())
    }

    pub fn append(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.finish_invalid()?;
        let result = self.append_valid(bytes);
        self.invalid |= result.is_err();
        result
    }

    fn append_valid(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.bytes += bytes.len();
        if self.bytes > 1024 * 1024 {
            return Err("trace: exceeds 1 MiB limit".into());
        }
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=end).collect::<Vec<_>>();
            let value: Value = serde_json::from_slice(&line).map_err(|_| "trace: malformed complete JSON record")?;
            self.record(value)?;
        }
        Ok(())
    }

    fn record(&mut self, value: Value) -> Result<(), String> {
        let name = value["event"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("trace: missing event")?;
        let elapsed = value["elapsed_ms"].as_u64().ok_or("trace: invalid timestamp")?;
        let id = value["run_id"].as_str().filter(|s| !s.is_empty());
        if id.is_none()
            && (name.starts_with("wyoming.")
                || matches!(
                    name,
                    "timing"
                        | "live.transcript"
                        | "tools/call"
                        | "session.delegation.created"
                        | "session.commentary.append"
                ))
        {
            return Err("trace: missing run ID".into());
        }
        if !value["run_id"].is_null() && id.is_none() {
            return Err("trace: invalid run ID".into());
        }
        if id.is_some_and(|s| s.len() > 64 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))) {
            return Err("trace: invalid run ID".into());
        }
        if name == "wyoming.mic_frames" && value["count"].as_u64().is_none() {
            return Err("trace: invalid microphone frame count".into());
        }
        let previous = self.timestamps.entry(id.unwrap_or("").to_owned()).or_default();
        if elapsed < *previous {
            return Err("trace: backwards timestamp".into());
        }
        *previous = elapsed;
        self.records.push(value);
        Ok(())
    }

    pub fn finish(&self) -> Result<(), String> {
        self.finish_invalid()?;
        if !self.pending.is_empty() {
            return Err("trace: incomplete final JSON line".into());
        }
        Ok(())
    }

    fn finish_invalid(&self) -> Result<(), String> {
        if self.invalid {
            return Err("trace: previously invalid trace".into());
        }
        Ok(())
    }

    pub fn partial(&self) -> bool {
        !self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::super::selectors;
    use super::*;
    use serde_json::json;
    #[test]
    fn periodic_probe_records_remain_raw_without_changing_journey_selectors() {
        let mut reader = Trace::default();
        let mut expected = Vec::new();
        for record in [
            json!({"event":"wyoming.run_started", "elapsed_ms":0, "run_id":"one"}),
            json!({"event":"wyoming.run_ended", "elapsed_ms":1, "run_id":"one"}),
        ] {
            let probe = json!({"event":"wyoming.probe_ok", "elapsed_ms":record["elapsed_ms"],
            "run_id":"connection-1", "provider_auth_mode":"api_key"});
            expected.extend([probe, record]);
        }
        for record in &expected {
            let bytes = format!("{record}\n").into_bytes();
            reader.append(&bytes[..bytes.len() - 1]).unwrap();
            assert!(reader.partial());
            reader.append(b"\n").unwrap();
        }
        let oracle = selectors::parse(&[json!("wyoming.run_started"), json!("wyoming.run_ended")]).unwrap();
        reader.finish().unwrap();
        assert_eq!(reader.records, expected);
        assert_eq!(selectors::check(&reader.records, &oracle, Some("one")), Ok(()));
    }
}
