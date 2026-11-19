//! Scalar identities are local wire ordinals, never invented bridge run IDs.
use echo_contracts::wyoming::Event;
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Receipt {
    pub direction: &'static str,
    pub kind: &'static str,
    pub elapsed_us: u128,
    pub pcm_bytes: usize,
    pub sequence: usize,
    pub wire_run: Option<usize>,
    pub playback_stream: Option<usize>,
}

#[derive(Clone, Default)]
pub(super) struct Ledger {
    pub receipts: Vec<Receipt>,
    runs: usize,
    streams: usize,
    run: Option<usize>,
    playing: Option<(usize, usize)>,
    pending: VecDeque<(usize, usize)>,
}

impl Ledger {
    pub fn observe(&mut self, direction: &'static str, event: &Event, elapsed_us: u128) -> Result<(), String> {
        let kind = match event.header.r#type.as_str() {
            "run-pipeline" => "run-pipeline",
            "audio-start" => "audio-start",
            "audio-chunk" => "audio-chunk",
            "audio-stop" => "audio-stop",
            "played" => "played",
            "hassmic-continue" => "hassmic-continue",
            "hassmic-flush" => "hassmic-flush",
            "hassmic-end" => "hassmic-end",
            _ => return Ok(()),
        };
        self.validate(direction, kind, event.payload.len(), elapsed_us)?;
        if direction == "capture" && kind == "run-pipeline" {
            self.runs += 1;
            self.run = Some(self.runs);
        }
        if direction == "playback" && kind == "audio-start" {
            self.streams += 1;
            self.playing = self.run.map(|run| (run, self.streams));
        }
        let identity = if direction == "capture" && kind == "played" {
            self.pending.pop_front()
        } else {
            self.playing
        };
        self.push(direction, kind, event.payload.len(), elapsed_us, identity);
        if direction == "playback" && matches!(kind, "audio-stop" | "hassmic-flush") {
            if let Some(identity) = self.playing.take() {
                self.pending.push_back(identity);
            }
        }
        if direction == "playback" && kind == "hassmic-end" {
            self.run = None;
        }
        Ok(())
    }

    fn push(
        &mut self,
        direction: &'static str,
        kind: &'static str,
        bytes: usize,
        elapsed_us: u128,
        identity: Option<(usize, usize)>,
    ) {
        self.receipts.push(Receipt {
            direction,
            kind,
            elapsed_us,
            pcm_bytes: if kind == "audio-chunk" { bytes } else { 0 },
            sequence: self.receipts.len() + 1,
            wire_run: if kind == "played"
                || direction == "playback"
                    && matches!(kind, "audio-start" | "audio-chunk" | "audio-stop" | "hassmic-flush")
            {
                identity.map(|pair| pair.0)
            } else {
                self.run
            },
            playback_stream: if direction == "playback" || kind == "played" {
                identity.map(|pair| pair.1)
            } else {
                None
            },
        });
    }

    fn validate(&self, direction: &str, kind: &str, bytes: usize, elapsed_us: u128) -> Result<(), String> {
        if self.receipts.len() >= 16384 {
            return Err("device: wire receipt limit exceeded".into());
        }
        let invalid = if !matches!(direction, "capture" | "playback") {
            Some("wire receipt direction")
        } else if self.receipts.last().is_some_and(|last| elapsed_us < last.elapsed_us) {
            Some("monotonic wire receipt clock")
        } else if kind == "audio-chunk" && (bytes == 0 || !bytes.is_multiple_of(2)) {
            Some("complete wire PCM samples")
        } else {
            None
        };
        invalid.map_or(Ok(()), |name| Err(format!("device: unmet prerequisite: {name}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_contracts::wyoming::Header;

    fn observe(ledger: &mut Ledger, direction: &'static str, kind: &str, at: u128) -> Receipt {
        ledger.observe(direction, &event(kind, vec![]), at).unwrap();
        ledger.receipts.last().unwrap().clone()
    }

    fn event(kind: &str, payload: Vec<u8>) -> Event {
        Event {
            header: Header {
                r#type: kind.into(),
                data: None,
                data_length: 0,
                payload_length: payload.len(),
            },
            data: None,
            payload,
        }
    }

    #[test]
    fn device_wire_ledger_correlates_two_streams_in_one_run_without_claiming_bridge_ids() {
        let mut ledger = Ledger::default();
        let wake = observe(&mut ledger, "capture", "run-pipeline", 0);
        assert_eq!((wake.sequence, wake.wire_run, wake.playback_stream), (1, Some(1), None));
        for stream in 1..=2 {
            let at = u128::try_from(stream).unwrap() * 10;
            let started = observe(&mut ledger, "playback", "audio-start", at);
            assert_eq!((started.wire_run, started.playback_stream), (Some(1), Some(stream)));
            observe(&mut ledger, "playback", "audio-stop", at + 1);
            let played = observe(&mut ledger, "capture", "played", at + 2);
            assert_eq!((played.wire_run, played.playback_stream), (Some(1), Some(stream)));
        }
        assert_eq!(observe(&mut ledger, "playback", "hassmic-end", 30).wire_run, Some(1));
        assert_eq!(observe(&mut ledger, "capture", "audio-stop", 31).wire_run, None);
        assert_eq!(ledger.receipts.len(), 9);
    }

    #[test]
    fn device_wire_ledger_late_played_retains_old_identity_and_unmatched_receipts_stay_missing() {
        let mut ledger = Ledger::default();
        assert_eq!(observe(&mut ledger, "capture", "played", 0).playback_stream, None);
        observe(&mut ledger, "capture", "run-pipeline", 1);
        observe(&mut ledger, "playback", "audio-start", 2);
        observe(&mut ledger, "playback", "hassmic-flush", 3);
        observe(&mut ledger, "playback", "audio-stop", 4);
        observe(&mut ledger, "playback", "hassmic-end", 5);
        observe(&mut ledger, "capture", "run-pipeline", 6);
        observe(&mut ledger, "playback", "audio-start", 7);
        let late = observe(&mut ledger, "capture", "played", 8);
        assert_eq!((late.wire_run, late.playback_stream), (Some(1), Some(1)));
        observe(&mut ledger, "playback", "audio-stop", 9);
        let current = observe(&mut ledger, "capture", "played", 10);
        assert_eq!((current.wire_run, current.playback_stream), (Some(2), Some(2)));
        assert_eq!(observe(&mut ledger, "capture", "played", 11).wire_run, None);
    }

    #[test]
    fn device_wire_ledger_rejects_backwards_malformed_and_overflow_sources_without_partial_receipt() {
        let mut ledger = Ledger::default();
        observe(&mut ledger, "capture", "run-pipeline", 100);
        for (direction, input, at, prerequisite) in [
            ("capture", event("played", vec![]), 99, "monotonic wire receipt clock"),
            (
                "capture",
                event("audio-chunk", vec![1]),
                101,
                "complete wire PCM samples",
            ),
            (
                "capture",
                event("audio-chunk", vec![]),
                101,
                "complete wire PCM samples",
            ),
            ("invalid", event("played", vec![]), 101, "wire receipt direction"),
        ] {
            assert_eq!(
                ledger.observe(direction, &input, at),
                Err(format!("device: unmet prerequisite: {prerequisite}"))
            );
            assert_eq!(ledger.receipts.len(), 1);
        }
        let valid = event("audio-chunk", vec![0, 0]);
        for _ in 1..16384 {
            ledger.observe("capture", &valid, 100).unwrap();
        }
        assert_eq!(
            ledger.observe("capture", &valid, 100),
            Err("device: wire receipt limit exceeded".into())
        );
        assert_eq!(ledger.receipts.len(), 16384);
    }
}
