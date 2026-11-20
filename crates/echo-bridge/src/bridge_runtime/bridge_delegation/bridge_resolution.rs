//! Retained-owner lifetime: a written creation is closed once, including drop.
use crate::{Error, bridge_trace::Trace};
use serde_json::{Value, json};
use tokio::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    Completed,
    Failed(&'static str),
    TimedOut,
    Cancelled,
}

pub(super) struct Resolution {
    trace: Trace,
    run: String,
    id: String,
    started: Instant,
    resolved: bool,
    observed: Outcome,
}

impl Resolution {
    pub fn created(trace: &Trace, run: &str, id: &str, task: &Value) -> Result<Self, Error> {
        let mut trace = trace.clone();
        trace.record(
            "session.delegation.created",
            Some(run),
            json!({"delegation_id":id,"task":task}),
        )?;
        Ok(Self {
            trace,
            run: run.into(),
            id: id.into(),
            started: Instant::now(),
            resolved: false,
            observed: Outcome::Cancelled,
        })
    }

    pub fn observed(&mut self, outcome: Outcome) {
        self.observed = outcome;
    }

    pub fn finish(&mut self, outcome: Outcome, handback: &'static str, answer: Option<&str>) -> Result<(), Error> {
        if self.resolved {
            return Ok(());
        }
        let (outcome, reason) = match outcome {
            Outcome::Completed => ("completed", None),
            Outcome::Failed(reason) => ("failed", Some(reason)),
            Outcome::TimedOut => ("timed_out", None),
            Outcome::Cancelled => ("cancelled", None),
        };
        let mut fields = json!({
            "delegation_id":self.id,"outcome":outcome,"handback":handback,
            "duration_ms":u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "tool_counts":{}
        });
        if let Some(reason) = reason {
            fields["reason_class"] = reason.into();
        }
        if let Some(text) = answer {
            fields["answer"] = json!({"text":text});
        }
        // A failed write must not turn cleanup into a second terminal attempt.
        self.resolved = true;
        self.trace.record("delegation.resolved", Some(&self.run), fields)
    }
}

impl Drop for Resolution {
    fn drop(&mut self) {
        // Drop cannot replace the owner's original error. Normal completion
        // propagates trace failures; shutdown still attempts this bounded write.
        let _ = self.finish(self.observed, "unobserved", None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test(start_paused = true)]
    async fn delegation_trace_retained_first_resolution_wins_over_late_finish_and_drop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let trace = Trace::open(&path, "api_key").unwrap();
        let mut lifetime = Resolution::created(&trace, "run-1", "delegation-1", &json!("task")).unwrap();
        tokio::time::advance(std::time::Duration::from_millis(23)).await;
        lifetime
            .finish(Outcome::Failed("task_failed"), "unobserved", None)
            .unwrap();
        lifetime
            .finish(Outcome::Completed, "submitted", Some("late answer"))
            .unwrap();
        drop(lifetime);
        let rows: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1],
            json!({"event":"delegation.resolved", "elapsed_ms":23,
            "provider_auth_mode":"api_key", "run_id":"run-1", "delegation_id":"delegation-1",
            "outcome":"failed", "reason_class":"task_failed", "handback":"unobserved",
            "duration_ms":23,"tool_counts":{}})
        );
        writeln!(std::io::stdout(), "{}", rows[1]).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn delegation_trace_retained_drop_keeps_observed_terminal_and_failed_creation_has_no_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let trace = Trace::open(&path, "api_key").unwrap();
        assert_eq!(
            Resolution::created(&trace, "run-1", &"x".repeat(4096), &json!({})).err(),
            Some(Error::Trace)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        let mut lifetime = Resolution::created(&trace, "run-2", "delegation-2", &json!({})).unwrap();
        lifetime.observed(Outcome::TimedOut);
        drop(lifetime);
        let rows: Vec<Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["outcome"], "timed_out");
        assert_eq!(rows[1]["handback"], "unobserved");
        assert!(rows[1].get("answer").is_none());
        assert_eq!(rows[1]["run_id"], "run-2");
        writeln!(std::io::stdout(), "{}", rows[1]).unwrap();
    }
}
