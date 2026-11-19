//! Match the existing finite M4 waveform, never infer an acoustic endpoint.
use super::ledger::{Ledger, Receipt};
use crate::lab_run::run_options::Options;
use echo_contracts::wyoming::{Event, WyomingEvent};
use serde_json::{Value, json};

#[derive(Clone)]
pub(super) struct Latency {
    signature: Vec<u8>,
    tail: Vec<u8>,
    endpoint: Option<u128>,
    run: Option<usize>,
}

impl Default for Latency {
    fn default() -> Self {
        // Independent reference for the existing M4 synthetic utterance.
        let mut state = 0x1234_5678_u32;
        let signature = (0..1600)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                if state & 1 == 0 { 1 } else { 2 }
            })
            .collect();
        Self {
            signature,
            tail: Vec::new(),
            endpoint: None,
            run: None,
        }
    }
}

impl Latency {
    pub fn observe(&mut self, receipt: &Receipt, event: &Event) -> Result<(), String> {
        if receipt.direction == "capture" && receipt.kind == "run-pipeline" {
            if self.run.is_some() {
                return Err("device: unmet prerequisite: isolated M4 wire run".into());
            }
            self.run = receipt.wire_run;
        }
        if receipt.kind != "audio-chunk" {
            return Ok(());
        }
        let rate = if receipt.direction == "capture" { 16000 } else { 24000 };
        if !matches!(event.typed(), Ok(WyomingEvent::AudioChunk(format))
            if format.rate == rate && format.width == 2 && format.channels == 1)
        {
            return Err("device: unmet prerequisite: M4 wire PCM format".into());
        }
        if receipt.direction != "capture" || self.endpoint.is_some() {
            return Ok(());
        }
        if self.run.is_none() || receipt.wire_run != self.run {
            return Err("device: unmet prerequisite: current-run M4 capture".into());
        }
        self.tail.extend(event.payload.as_chunks::<2>().0.iter().map(|bytes| {
            let sample = i16::from_le_bytes(*bytes);
            u8::from(sample > 0) + 2 * u8::from(sample < 0)
        }));
        if self
            .tail
            .windows(self.signature.len())
            .any(|window| window == self.signature)
        {
            self.endpoint = Some(receipt.elapsed_us);
            self.tail.clear();
        } else {
            let discard = self.tail.len().saturating_sub(self.signature.len() - 1);
            self.tail.drain(..discard);
        }
        Ok(())
    }
}

pub(super) fn ceiling(options: &Options) -> Result<Option<f64>, String> {
    let mut limit = None;
    for path in &options.scenarios {
        let scenario = crate::lab_validate::load(path)?;
        if scenario["id"] == "m4-spoof-tool" {
            if options.scenarios.len() != 1 {
                return Err("device: unmet prerequisite: isolated M4 scenario".into());
            }
            limit = Some(
                scenario["budgets"]["first_audio_after_endpoint_ms_p95"]
                    .as_f64()
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .ok_or("device: invalid M4 latency ceiling")?,
            );
        }
    }
    Ok(limit)
}

pub(super) fn check(ledger: &Ledger, ceiling: f64) -> Result<Value, String> {
    if !ceiling.is_finite() || ceiling < 0.0 {
        return Err("device: invalid M4 latency ceiling".into());
    }
    if let Some(error) = &ledger.failure {
        return Err(error.clone());
    }
    let latency = ledger
        .latency
        .as_ref()
        .ok_or("device: unmet prerequisite: M4 latency observer")?;
    let run = latency.run.ok_or("device: unmet prerequisite: M4 wire run")?;
    let endpoint = latency
        .endpoint
        .ok_or("device: unmet prerequisite: matched finite M4 utterance endpoint")?;
    let first = ledger
        .session
        .first_reply_pcm(run)
        .ok_or("device: unmet prerequisite: M4 reply PCM")?;
    let elapsed = first
        .checked_sub(endpoint)
        .ok_or("device: unmet prerequisite: M4 reply after endpoint")?;
    let measured = p95(&[elapsed])?;
    if measured > ceiling {
        return Err(format!(
            "device: first_audio_after_endpoint_ms_p95 observed {measured} ms exceeds {ceiling} ms"
        ));
    }
    Ok(
        json!({"event":"device.budget","scenario":"m4-spoof-tool","wire_run":run,
        "name":"first_audio_after_endpoint_ms_p95","measured":measured,"limit":ceiling,
        "unit":"ms","rule":"<=","source":"matched capture+post-write reply PCM",
        "endpoint_us":endpoint,"first_reply_pcm_us":first,"passes":true}),
    )
}

fn p95(samples: &[u128]) -> Result<f64, String> {
    if samples.is_empty() {
        return Err("device: unmet prerequisite: M4 latency samples".into());
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let micros = ordered[ordered.len() - ordered.len() / 20 - 1];
    let micros = u64::try_from(micros).map_err(|_| "device: malformed M4 latency duration")?;
    Ok(std::time::Duration::from_micros(micros).as_secs_f64() * 1000.0)
}

#[cfg(test)]
#[path = "latency_tests.rs"]
mod tests;
