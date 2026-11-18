//! Failure-only projection: never copy arbitrary trace fields or child stderr.
use super::Trace;
use serde_json::{Value, json};

fn event_name(record: &Value) -> &str {
    record["event"]
        .as_str()
        .filter(|name| {
            matches!(
                *name,
                "bridge.started"
                    | "bridge.error"
                    | "bridge.stopped"
                    | "wyoming.probe_ok"
                    | "wyoming.run_started"
                    | "wyoming.run_ended"
                    | "wyoming.mic_frames"
                    | "live.closed"
                    | "live.connected"
                    | "screen.listening"
                    | "bridge.ready"
                    | "bridge.run_completed"
                    | "run.started"
                    | "mic.completed"
                    | "reply.played"
                    | "call.start_failed"
                    | "call.control_failed"
                    | "call.media_failed"
            )
        })
        .unwrap_or("redacted")
}

fn safe(record: &Value) -> Value {
    let event = event_name(record);
    let mut row = json!({"event":event});
    if let Some(elapsed) = record["elapsed_ms"].as_u64() {
        row["elapsed_ms"] = json!(elapsed);
    }
    if event.starts_with("call.") {
        row["stage"] = json!(
            record["stage"]
                .as_str()
                .filter(|stage| matches!(
                    *stage,
                    "new" | "signaling_started" | "answer_received" | "answer_applied" | "media_ready"
                ))
                .unwrap_or("unavailable or redacted")
        );
        row["media_state"] = json!(
            record["media_state"]
                .as_str()
                .filter(|state| matches!(*state, "New" | "Connecting" | "Connected" | "Failed" | "Closed"))
                .unwrap_or("unavailable or redacted")
        );
    }
    if event == "bridge.error" {
        row["reason"] = json!(
            record["reason"]
                .as_str()
                .filter(|reason| matches!(
                    *reason,
                    "wyoming"
                        | "deadline"
                        | "control"
                        | "authorization"
                        | "audio"
                        | "decode"
                        | "trace"
                        | "health"
                        | "screen"
                        | "unavailable"
                        | "invalid input"
                ))
                .unwrap_or("unavailable or redacted")
        );
    }
    row
}

pub(super) fn summary(status: &str, trace: &Trace) -> String {
    let tail: Vec<_> = trace.records.iter().rev().take(12).map(safe).collect();
    let error = trace
        .records
        .iter()
        .rev()
        .find(|row| row["event"] == "bridge.error")
        .map(safe);
    let completeness = if trace.records.is_empty() {
        "unavailable"
    } else if trace.finish().is_err() {
        "incomplete or invalid"
    } else {
        "complete"
    };
    format!(
        "bridge diagnostics: {}",
        json!({"exit":status,"trace":completeness,
        "bridge_error":error,"tail_newest_first":tail})
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m1_restart_diagnostics_redacts_payloads_and_marks_incomplete_trace() {
        let mut trace = Trace::default();
        for event in ["bridge.error", "synthetic-secret-sentinel"] {
            trace
                .append(
                    format!(
                        "{}\n",
                        json!({"event":event,"elapsed_ms":1,
                "reason":"wyoming","payload":"synthetic-secret-sentinel"})
                    )
                    .as_bytes(),
                )
                .unwrap();
        }
        trace.append(b"{\"unfinished\":").unwrap();
        let text = summary("exit status: 7", &trace);
        assert!(text.contains("exit status: 7"));
        assert!(text.contains("bridge.error"));
        assert!(text.contains("wyoming"));
        assert!(text.contains("incomplete or invalid"));
        assert!(!text.contains("synthetic-secret-sentinel"));
        assert!(summary("unavailable", &Trace::default()).contains("\"bridge_error\":null"));
        assert_eq!(
            safe(&json!({"event":"bridge.error","reason":"synthetic-secret-sentinel"})),
            json!({"event":"bridge.error","reason":"unavailable or redacted"})
        );
        assert_eq!(
            safe(&json!({"event":"call.media_failed", "stage":"media_ready",
            "media_state":"Failed", "payload":"synthetic-secret-sentinel"})),
            json!({"event":"call.media_failed", "stage":"media_ready", "media_state":"Failed"})
        );
        assert_eq!(
            safe(
                &json!({"event":"call.control_failed", "stage":"synthetic-secret-sentinel",
            "media_state":"synthetic-secret-sentinel"})
            ),
            json!({"event":"call.control_failed", "stage":"unavailable or redacted", "media_state":"unavailable or redacted"})
        );
        for _ in 0..20 {
            trace.records.push(json!({"event":"wyoming.probe_ok","elapsed_ms":2}));
        }
        let text = summary("signal: 9", &trace);
        let value: Value = serde_json::from_str(text.strip_prefix("bridge diagnostics: ").unwrap()).unwrap();
        assert_eq!(value["tail_newest_first"].as_array().unwrap().len(), 12);
        assert_eq!(value["bridge_error"]["reason"], "wyoming");
        assert_eq!(value["exit"], "signal: 9");
    }
}
