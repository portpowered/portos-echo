//! Final session verdicts use one post-write wire clock, never trace-clock subtraction.
use super::ledger::{Ledger, Receipt};
use crate::lab_run::run_options::Options;
use serde_json::{Value, json};

pub(super) struct Expected {
    replies: usize,
    listen_seconds: u64,
}

impl Expected {
    pub fn load(options: &Options) -> Result<Option<Self>, String> {
        let mut expected = None;
        for path in &options.scenarios {
            let scenario = crate::lab_validate::load(path)?;
            let replies = match scenario["id"].as_str() {
                Some("session-two-turns") => 2,
                Some("session-idle-timeout") => 1,
                _ => continue,
            };
            if options.scenarios.len() != 1 {
                return Err("device: unmet prerequisite: isolated session scenario".into());
            }
            expected = Some(Self {
                replies,
                listen_seconds: scenario["budgets"]["session_listen_seconds"]
                    .as_u64()
                    .filter(|seconds| *seconds > 0)
                    .ok_or("device: unmet prerequisite: session listen setting")?,
            });
        }
        Ok(expected)
    }

    pub fn check(&self, ledger: &Ledger) -> Result<Value, String> {
        if let Some(error) = &ledger.failure {
            return Err(error.clone());
        }
        let replies = ledger.session.settled(self.replies)?;
        let last = replies
            .last()
            .ok_or("device: unmet prerequisite: last activity Played")?
            .1;
        let end = unique_end(&ledger.receipts)?;
        let capture = capture_between_replies(&ledger.receipts, &replies)?;
        let idle_us = end
            .checked_sub(last)
            .ok_or("device: unmet prerequisite: ordered idle end")?;
        let expected_us = u128::from(self.listen_seconds) * 1_000_000;
        let offset_us = idle_us.abs_diff(expected_us);
        if offset_us > 100_000 {
            return Err(format!(
                "device: session idle expected {} ms +/-100 ms, observed {} us; offset={}{} us",
                expected_us / 1000,
                idle_us,
                if idle_us < expected_us { "-" } else { "+" },
                offset_us
            ));
        }
        Ok(
            json!({"event":"device.session_timing","source":"post-write wire receipts",
            "wire_run":1,"replies":replies.len(),"session_listen_seconds":self.listen_seconds,
            "last_activity_played_us":last,"hassmic_end_us":end,"idle_after_played_us":idle_us,
            "idle_offset_us":offset_us,"capture_chunks_between_replies":capture}),
        )
    }
}

fn unique_end(receipts: &[Receipt]) -> Result<u128, String> {
    let ends = receipts
        .iter()
        .filter(|r| r.direction == "playback" && r.kind == "hassmic-end")
        .collect::<Vec<_>>();
    let [end] = ends.as_slice() else {
        return Err("device: unmet prerequisite: one hassmic-end receipt".into());
    };
    if end.wire_run != Some(1) {
        return Err("device: unmet prerequisite: current-run hassmic-end receipt".into());
    }
    Ok(end.elapsed_us)
}

fn capture_between_replies(receipts: &[Receipt], replies: &[(usize, u128)]) -> Result<usize, String> {
    let Some(second) = replies.get(1) else { return Ok(0) };
    let first = receipts
        .iter()
        .find(|r| r.direction == "capture" && r.kind == "played" && r.playback_stream == Some(replies[0].0))
        .ok_or("device: unmet prerequisite: first reply Played receipt")?;
    let start = receipts
        .iter()
        .find(|r| r.direction == "playback" && r.kind == "audio-start" && r.playback_stream == Some(second.0))
        .ok_or("device: unmet prerequisite: second reply stream")?;
    let count = receipts
        .iter()
        .filter(|r| {
            r.direction == "capture"
                && r.kind == "audio-chunk"
                && r.wire_run == Some(1)
                && r.sequence > first.sequence
                && r.sequence < start.sequence
        })
        .count();
    if count == 0 {
        return Err("device: unmet prerequisite: capture between session replies".into());
    }
    Ok(count)
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
