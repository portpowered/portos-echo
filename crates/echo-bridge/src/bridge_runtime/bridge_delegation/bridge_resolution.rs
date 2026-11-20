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
    pub fn created(trace: &Trace, run: &str, id: &str, task: Value) -> Result<Self, Error> {
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
