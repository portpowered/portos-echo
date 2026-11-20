//! Authored events and counters, with bounded redacted delegation payloads.
//! `settings.saved` records only `restart_pending` after a committed save;
//! `prompt.saved` records only the fixed voice|runner selector. A generation
//! records `settings.reexec_pending` once when its first changed save commits.
//! `settings.ignored` uses fixed reason `invalid_config` for an unusable overlay.
//! Cleanup publishes `settings.restart_prepared`; only final EOF/kill admission
//! publishes `settings.reexec_attempt`. A failed replacement records
//! `settings.reexec_failed` with reason exec. No settings values, paths or text.
//! `prompt.defaulted` names voice|runner once per missing kind per runtime.
//! `prompt.reload_refused` adds a fixed reason; neither contains paths or text.
//! `wyoming.probe_ok` acknowledges each successful idle describe probe when its
//! owner observes completion. It carries only the existing envelope, no payload.
//! `reply.cut` also reports `bridge_evicted` (prior capacity evictions) and
//! `reply_frames` (complete decoded frames offered to the bridge queue). Neither
//! prior evictions nor partial tails belong to the retained `dropped_frames`.
//! `reply.overflow` records the first bridge reply eviction per run, with its
//! fixed frame capacity. Final eviction totals remain in `reply.cut`.
//! Release events acknowledge acquired listener owners after shutdown is joined.
//! Voice runtime `screen.released` also counts clients dropped at the drain bound.
//! `reply.quality` counts in `cuts` the barge-ins that cut the reply, with the
//! `trigger` of the last: `user_transcript`, `speech_started` or null, in
//! `holds` the times the Live session's sign of user speech held the reply
//! back from the Echo, and in `hold_ms` for how long in all. For the last cut,
//! `silent_ms` runs from that sign to the last reply frame the Echo played (by
//! the modelled playout queue), `cut_ms` from the sign to the cut, and `text`
//! is the session's transcript of the user under it, at most 240 bytes: it
//! shows whether the session heard the user or the Echo's own speaker. Its
//! `mic` is a passive measurement of the microphone under the reply and
//! decides nothing: `onsets` of a level the reply's echo did not explain,
//! the loudest frame taken for echo (`echo_max`) and
//! of all (`level_max`) as RMS, `coupling`, the echo per mille of reply level,
//! `echo_mean`, the mean of that ratio over this reply's loud frames, and
//! `noise`, the room's RMS with the speaker silent.
//! Its `aec` is what the echo canceller measured under the reply, or null
//! with `audio.echo_cancel` off: `delay_ms`, the delay AEC3 found from a
//! reply frame's modelled playout to its echo at the microphone, `erle_db`,
//! the microphone's energy over the uplink's while reply audio could be
//! heard, and `converged`, true once that reduction reached 15 dB.
//! A reply a barge-in cut settles with `reply.played` `outcome` `cut`.
//! `timing` follows a settled reply's `reply.quality` when that reply has a
//! sample: `name` is `first_audio_after_endpoint_ms`, `duration_ms` is whole
//! milliseconds on the bridge clock from the endpoint to the first reply frame
//! handed to the Echo, and `endpoint` is `user_transcript` or `delegated_turn`,
//! the Live session's first sign that the user's turn was over.
use crate::Error;
use serde_json::{Map, Value};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::time::Instant;
mod delegation_payload;
mod native_delegation;
mod native_tools;
mod tool_receipts;
pub(crate) use tool_receipts::{PortosReceipts, ToolTrace};

#[derive(Clone)]
pub struct Trace {
    writer: Arc<Mutex<File>>,
    started: Instant,
    mode: &'static str,
    active_run: Arc<Mutex<Option<String>>>,
    payloads: bool,
}

impl Trace {
    /// # Errors
    /// Returns a safe I/O category; paths never enter diagnostics.
    pub fn open(path: &Path, mode: &'static str) -> Result<Self, Error> {
        let mut options = OpenOptions::new();
        let writer = crate::bridge_private::file(options.create(true).append(true))
            .open(path)
            .map_err(|_| Error::Trace)?;
        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            started: Instant::now(),
            mode,
            active_run: Arc::default(),
            payloads: true,
        })
    }

    pub(crate) fn with_payloads(mut self, payloads: bool) -> Self {
        self.payloads = payloads;
        self
    }

    pub(crate) fn tool_run(&self, run: Option<&str>) -> Result<(), Error> {
        *self.active_run.lock().map_err(|_| Error::Trace)? = run.map(str::to_owned);
        Ok(())
    }

    /// Callers supply authored event names and safe metadata. Delegation payload
    /// fields are redacted, capped or removed here before reaching the writer.
    /// # Errors
    /// A serialization/write/flush failure prevents claiming completion.
    pub fn record(&mut self, event: &str, run: Option<&str>, fields: Value) -> Result<(), Error> {
        let mut writer = self.writer.lock().map_err(|_| Error::Trace)?;
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
        if let Value::Object(mut fields) = fields {
            if delegation_payload::event(event) {
                delegation_payload::prepare(&mut fields, self.payloads)?;
            }
            entry.extend(fields);
        }
        if delegation_payload::event(event) && !delegation_payload::metadata_fits(&entry)? {
            return Err(Error::Trace);
        }
        let bytes = serde_json::to_vec(&entry).map_err(|_| Error::Trace)?;
        let limit = if delegation_payload::event(event) {
            2 * 65536 + 4096
        } else {
            4096
        };
        if bytes.len() > limit {
            return Err(Error::Trace);
        }
        writer
            .write_all(&bytes)
            .and_then(|()| writer.write_all(b"\n"))
            .and_then(|()| writer.flush())
            .map_err(|_| Error::Trace)
    }
}

#[cfg(test)]
mod bridge_spine_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(start_paused = true)]
    async fn bridge_trace_release_receipts_have_fixed_envelope_and_drop_count() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        for mode in ["chatgpt", "api_key"] {
            let mut trace = Trace::open(&path, mode).unwrap();
            for event in ["health.released", "screen.released"] {
                let fields = if event == "screen.released" {
                    json!({"dropped_clients":3})
                } else {
                    json!({})
                };
                trace.record(event, None, fields).unwrap();
            }
        }
        let rows: Vec<Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            rows,
            ["chatgpt", "api_key"]
                .into_iter()
                .flat_map(|mode| {
                    ["health.released", "screen.released"].map(|event| {
                        let mut row = json!({"event":event,"elapsed_ms":0,"provider_auth_mode":mode});
                        if event == "screen.released" {
                            row["dropped_clients"] = json!(3);
                        }
                        row
                    })
                })
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn bridge_trace_probe_success_has_only_fixed_envelope() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        tokio::time::advance(std::time::Duration::from_millis(7)).await;
        trace
            .record("wyoming.probe_ok", Some("connection-1"), json!({}))
            .unwrap();
        let bytes = std::fs::read(path).unwrap();
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({
                "event":"wyoming.probe_ok", "elapsed_ms":7,
                "provider_auth_mode":"api_key", "run_id":"connection-1"
            })
        );
    }

    #[test]
    fn bridge_spine_trace_write_failure_is_sanitized_and_keeps_existing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        std::fs::write(&path, b"prior record\n").unwrap();
        let mut trace = Trace {
            writer: Arc::new(Mutex::new(File::open(&path).unwrap())), // Read-only sink injects an actual write error.
            started: Instant::now(),
            mode: "chatgpt",
            active_run: Arc::default(),
            payloads: true,
        };
        let error = trace.record("run.started", Some("run-1"), json!({})).unwrap_err();
        assert_eq!(error, Error::Trace);
        assert_eq!(error.to_string(), "trace");
        assert_eq!(std::fs::read(path).unwrap(), b"prior record\n");
    }

    #[test]
    fn bridge_spine_oversized_trace_is_rejected_before_any_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        assert_eq!(
            trace.record("run.started", Some("run-1"), json!({"reason":"x".repeat(4096)})),
            Err(Error::Trace)
        );
        assert_eq!(std::fs::read(&path).unwrap(), Vec::<u8>::new());
        trace.record("run.started", Some("run-1"), json!({})).unwrap();
        let line: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(line["provider_auth_mode"], "api_key");
        assert_eq!(line["event"], "run.started");
        assert_eq!(line["run_id"], "run-1");
    }

    #[test]
    fn bridge_spine_cloned_trace_writers_keep_complete_records_and_one_timeline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace");
        let trace = Trace::open(&path, "api_key").unwrap();
        std::thread::scope(|scope| {
            let mut workers = vec![];
            for writer in 0..4 {
                let mut trace = trace.clone();
                workers.push(scope.spawn(move || {
                    for sequence in 0..64 {
                        trace
                            .record(
                                "mcp.refresh.committed",
                                None,
                                json!({"writer":writer,"sequence":sequence}),
                            )
                            .unwrap();
                    }
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let entries: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(entries.len(), 256);
        for writer in 0..4 {
            let sequences: Vec<_> = entries
                .iter()
                .filter(|e| e["writer"] == writer)
                .map(|e| e["sequence"].as_u64().unwrap())
                .collect();
            assert_eq!(sequences, (0..64).collect::<Vec<_>>());
        }
        assert!(
            entries
                .iter()
                .all(|e| e["event"] == "mcp.refresh.committed" && e["provider_auth_mode"] == "api_key")
        );
        assert!(
            entries
                .windows(2)
                .all(|e| e[0]["elapsed_ms"].as_u64() <= e[1]["elapsed_ms"].as_u64())
        );
    }
}
