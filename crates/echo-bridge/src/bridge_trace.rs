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
//! `prompt.applied` names voice|runner at each call admission with an 8-hex
//! SHA-256 prefix `digest` of the admitted text and `changed`; never the text.
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
//! A reply a barge-in cut settles with `reply.played` `outcome` `cut`.
//! `timing` follows a settled reply's `reply.quality` when that reply has a
//! sample: `name` is `first_audio_after_endpoint_ms`, `duration_ms` is whole
//! milliseconds on the bridge clock from the endpoint to the first reply frame
//! handed to the Echo, and `endpoint` is `user_transcript` or `delegated_turn`,
//! the Live session's first sign that the user's turn was over.
use crate::{Error, bridge_world::Controller};
use echo_core_api::{PendingTrace, TraceDrain, TraceRow};
use echo_svc_trace::Writer;
use serde_json::Value;
use std::{
    path::Path,
    sync::{Arc, mpsc::sync_channel},
};
use tokio::time::Instant;
mod delegation_payload;
mod native_delegation;
mod native_tools;
mod tool_receipts;
pub(crate) use tool_receipts::{PortosReceipts, ToolTrace};

#[derive(Clone)]
pub struct Trace {
    owner: Arc<Owner>,
    started: Instant,
    mode: &'static str,
    payloads: bool,
}

struct Owner {
    world: Arc<Controller>,
    writer: Arc<Writer>,
    ticker: Option<tokio::task::JoinHandle<()>>,
    tick_needed: Arc<tokio::sync::Notify>,
}

impl Owner {
    fn finish(&self) -> Result<(), Error> {
        self.world.close_trace();
        if let Some(ticker) = &self.ticker {
            ticker.abort();
        }
        self.writer.finish().map_err(|_| {
            self.world.write_failed();
            Error::Trace
        })
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn ticker(
    world: Arc<Controller>,
    writer: &Arc<Writer>,
    tick_needed: Arc<tokio::sync::Notify>,
) -> Option<tokio::task::JoinHandle<()>> {
    let handle = tokio::runtime::Handle::try_current().ok()?;
    let writer = Arc::downgrade(writer);
    Some(handle.spawn(async move {
        loop {
            tick_needed.notified().await;
            if let Some(deadline) = world.trace_deadline() {
                tokio::time::sleep_until(deadline).await;
                let Some(writer) = writer.upgrade() else { break };
                if world.trace_tick() {
                    writer.wake();
                }
            }
        }
    }))
}

impl Trace {
    /// # Errors
    /// Returns a safe I/O category; paths never enter diagnostics.
    pub fn open(path: &Path, mode: &'static str) -> Result<Self, Error> {
        Self::open_in(path, mode, Controller::new())
    }

    pub(crate) fn open_in(path: &Path, mode: &'static str, world: Arc<Controller>) -> Result<Self, Error> {
        let writer = Arc::new(Writer::open(path, world.clone()).map_err(|_| {
            world.write_failed();
            Error::Trace
        })?);
        let started = Instant::now();
        let tick_needed = Arc::new(tokio::sync::Notify::new());
        let ticker = ticker(world.clone(), &writer, tick_needed.clone());
        Ok(Self {
            owner: Arc::new(Owner {
                world,
                writer,
                ticker,
                tick_needed,
            }),
            started,
            mode,
            payloads: true,
        })
    }

    pub(crate) fn open_for_voice(
        path: &Path,
        mode: echo_config::LiveMode,
        world: Option<Arc<Controller>>,
        config: Option<&echo_config::BridgeConfig>,
    ) -> Result<Self, Error> {
        let mode = match mode {
            echo_config::LiveMode::Chatgpt => "chatgpt",
            echo_config::LiveMode::ApiKey => "api_key",
        };
        let trace = Self::open_in(path, mode, world.unwrap_or_else(Controller::new))?;
        Ok(trace.with_payloads(config.is_none_or(|config| config.trace.payloads)))
    }

    pub(crate) fn finish(&self) -> Result<(), Error> {
        self.owner.finish()
    }

    pub(crate) fn with_payloads(mut self, payloads: bool) -> Self {
        self.payloads = payloads;
        self
    }

    pub(crate) fn current_run(&self) -> Result<Option<String>, Error> {
        self.owner.world.current_run()
    }

    pub(crate) fn tool_run(&self, run: Option<&str>) -> Result<(), Error> {
        self.owner.world.set_run(run)
    }

    /// Callers supply authored event names and safe metadata. Delegation payload
    /// fields are redacted, capped or removed here before reaching the writer.
    /// # Errors
    /// A serialization/write/flush failure prevents claiming completion.
    pub fn record(&mut self, event: &str, run: Option<&str>, fields: Value) -> Result<(), Error> {
        let (receipt, completion) = sync_channel(1);
        self.enqueue(PendingTrace {
            row: self.row(event, run, fields)?,
            receipt: Some(receipt),
        })?;
        completion.recv().map_err(|_| Error::Trace)?.map_err(|_| Error::Trace)
    }

    /// Periodic clock diagnostics have no dependent effect or completion receipt.
    pub(crate) fn buffer_clock_offset(&self, fields: Value) -> Result<(), Error> {
        self.enqueue(PendingTrace {
            row: self.row("clock.offset", None, fields)?,
            receipt: None,
        })
    }

    fn enqueue(&self, pending: PendingTrace) -> Result<(), Error> {
        if self.owner.world.enqueue(pending, self.started)? {
            self.owner.writer.wake();
        } else if self.owner.ticker.is_some() {
            self.owner.tick_needed.notify_one();
        } else {
            self.owner.writer.wake();
        }
        Ok(())
    }

    fn row(&self, event: &str, run: Option<&str>, fields: Value) -> Result<TraceRow, Error> {
        let delegation = delegation_payload::event(event);
        let mut fields = match fields {
            Value::Object(fields) => fields,
            _ => serde_json::Map::new(),
        };
        if delegation {
            delegation_payload::prepare(
                &mut fields,
                self.payloads && !crate::bridge_endpoint::remote_start::remote(),
            )?;
        }
        Ok(TraceRow {
            event: event.to_owned(),
            elapsed_ms: 0,
            mode: self.mode,
            run: run.map(str::to_owned),
            fields,
            payload_fields: if delegation { &delegation_payload::FIELDS } else { &[] },
            limit: if delegation {
                2 * delegation_payload::CAP + 4096
            } else {
                4096
            },
        })
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

    #[tokio::test(start_paused = true)]
    async fn bridge_trace_a_lone_buffered_row_drains_at_the_paused_tick() {
        use echo_core_api::{Clock, SystemClock};
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let trace = Trace::open(&path, "api_key").unwrap();
        trace
            .buffer_clock_offset(json!({"synced":false,"reason":"unreachable"}))
            .unwrap();
        tokio::task::yield_now().await;
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        tokio::time::advance(echo_sub_trace::TICK).await;
        tokio::task::yield_now().await;
        let deadline = SystemClock.now().monotonic + Duration::from_secs(2);
        let expected = b"{\"elapsed_ms\":0,\"event\":\"clock.offset\",\"provider_auth_mode\":\"api_key\",\"reason\":\"unreachable\",\"synced\":false}\n";
        loop {
            if std::fs::read(&path).unwrap() == expected {
                break;
            }
            assert!(
                SystemClock.now().monotonic < deadline,
                "row was not flushed on the first trace tick"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(trace.started.elapsed(), echo_sub_trace::TICK);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bridge_spine_trace_write_failure_is_sanitized() {
        let mut trace = Trace::open(Path::new("/dev/full"), "chatgpt").unwrap();
        let error = trace.record("run.started", Some("run-1"), json!({})).unwrap_err();
        assert_eq!(error, Error::Trace);
        assert_eq!(error.to_string(), "trace");
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
