//! Independent speaker receipts. Neither a trace nor a wake chime is a reply.
use super::ledger::Receipt;
use echo_contracts::wyoming::Event;
use serde_json::{Value, json};

#[derive(Clone, Default)]
pub(super) struct Session {
    runs: Vec<usize>,
    ended: Vec<usize>,
    streams: Vec<Stream>,
    capture_after_reply: usize,
    unmatched_played: usize,
}

#[derive(Clone, Default)]
struct Stream {
    run: Option<usize>,
    id: usize,
    pcm_bytes: usize,
    audible_samples: usize,
    chime: Option<bool>,
    stopped: Option<u128>,
    ending: Option<&'static str>,
    played: Option<u128>,
    first_pcm: Option<u128>,
}

impl Stream {
    fn pcm(&mut self, event: &Event) -> Result<(), String> {
        let recognized = chime(&event.payload, self.pcm_bytes / 2);
        if self.chime == Some(true) && !recognized {
            return Err("device: unmet prerequisite: complete wake chime waveform".into());
        }
        self.chime.get_or_insert(recognized);
        self.pcm_bytes += event.payload.len();
        self.audible_samples += event
            .payload
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|sample| i16::from_le_bytes(**sample).unsigned_abs() > 64)
            .count();
        Ok(())
    }

    fn reply(&self) -> bool {
        self.chime == Some(false) && self.pcm_bytes > 0 && self.audible_samples > 0
    }
}

impl Session {
    pub fn observe(&mut self, receipt: &Receipt, event: &Event) -> Result<(), String> {
        match (receipt.direction, receipt.kind) {
            ("capture", "run-pipeline") => self.runs.extend(receipt.wire_run),
            ("playback", "hassmic-end") => self.ended.extend(receipt.wire_run),
            ("playback", "audio-start") => self.streams.push(Stream {
                run: receipt.wire_run,
                id: receipt
                    .playback_stream
                    .ok_or("device: unmet prerequisite: playback stream identity")?,
                ..Stream::default()
            }),
            ("capture", "audio-chunk") if self.streams.iter().any(|s| s.reply() && s.played.is_some()) => {
                self.capture_after_reply += 1;
            }
            ("capture", "played") if receipt.playback_stream.is_none() => self.unmatched_played += 1,
            _ => self.stream_event(receipt, event)?,
        }
        Ok(())
    }

    fn stream_event(&mut self, receipt: &Receipt, event: &Event) -> Result<(), String> {
        let Some(stream) = self
            .streams
            .iter_mut()
            .find(|stream| Some(stream.id) == receipt.playback_stream && stream.run == receipt.wire_run)
        else {
            if receipt.direction == "playback" && receipt.kind == "audio-chunk" {
                return Err("device: unmet prerequisite: current playback PCM identity".into());
            }
            return Ok(());
        };
        match (receipt.direction, receipt.kind) {
            ("playback", "audio-chunk") => {
                if stream.stopped.is_some() {
                    return Err("device: old reply PCM after audio-stop or hassmic-flush".into());
                }
                stream.first_pcm.get_or_insert(receipt.elapsed_us);
                stream.pcm(event)?;
            }
            ("playback", "audio-stop" | "hassmic-flush") => {
                // A drained reply may be flushed before Played. Keep the
                // cut identity even when a redundant audio-stop follows it.
                if stream.ending != Some("hassmic-flush") {
                    stream.stopped = Some(receipt.elapsed_us);
                    stream.ending = Some(receipt.kind);
                }
            }
            ("capture", "played") => stream.played = Some(receipt.elapsed_us),
            _ => (),
        }
        Ok(())
    }

    pub fn reply_barrier(&self, run: usize, replies: usize, final_reply: bool) -> Result<bool, String> {
        // A one-reply milestone may end immediately after Played, before its
        // waiter is scheduled. Persistent next-turn barriers still require an
        // open run. Both paths require the independent settled reply identity.
        if final_reply && run == 1 && !self.ended.is_empty() {
            return self.settled(replies).map(|_| true);
        }
        self.checkpoint(run, replies)
    }

    pub fn checkpoint(&self, run: usize, replies: usize) -> Result<bool, String> {
        if self.unmatched_played != 0 || self.runs.iter().any(|id| *id != run) {
            return Err("device: unmet prerequisite: current-run Played identity".into());
        }
        let played = self
            .streams
            .iter()
            .filter(|s| s.run == Some(run) && s.reply() && s.ending == Some("audio-stop") && s.played.is_some())
            .count();
        if self.ended.contains(&run) || played > replies {
            return Err("device: session advanced beyond reply barrier".into());
        }
        Ok(played == replies && self.runs == [run])
    }

    pub fn settled(&self, replies: usize) -> Result<Vec<(usize, u128)>, String> {
        if self.runs != [1] || self.ended != [1] || self.unmatched_played != 0 {
            return Err("device: unmet prerequisite: one complete wire session identity".into());
        }
        let completed = self
            .streams
            .iter()
            .filter(|stream| stream.run == Some(1) && stream.reply())
            .map(|stream| {
                let stopped = stream.stopped.ok_or("device: unmet prerequisite: reply audio-stop")?;
                let played = stream.played.ok_or("device: unmet prerequisite: reply Played")?;
                if stream.ending != Some("audio-stop") || played < stopped {
                    return Err("device: unmet prerequisite: ordered settled reply".into());
                }
                Ok((stream.id, played))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if completed.len() != replies {
            return Err(format!(
                "device: unmet prerequisite: settled reply count; expected {replies}, observed {}",
                completed.len()
            ));
        }
        Ok(completed)
    }

    pub fn observation(&self) -> Value {
        let streams = self
            .streams
            .iter()
            .map(|s| {
                json!({
                    "wire_run":s.run,"playback_stream":s.id,"pcm_bytes":s.pcm_bytes,
                    "audible_samples":s.audible_samples,"wake_chime":s.chime,
                    "stopped_us":s.stopped,"ending":s.ending,"played_us":s.played
                })
            })
            .collect::<Vec<_>>();
        json!({"event":"device.wire_session","wire_runs":self.runs,"ended_runs":self.ended,
            "capture_chunks_after_reply":self.capture_after_reply,
            "unmatched_played":self.unmatched_played,"streams":streams})
    }

    pub fn first_reply_pcm(&self, run: usize) -> Option<u128> {
        self.streams
            .iter()
            .find(|stream| stream.run == Some(run) && stream.reply())
            .and_then(|stream| stream.first_pcm)
    }
}

// Independent reference for the existing 160 ms, two-note 24 kHz wake chime.
// Compare signed samples; quiet or arbitrary PCM cannot qualify as a chime.
fn chime(bytes: &[u8], offset: usize) -> bool {
    if bytes.len() != 960 || offset + 480 > 3840 {
        return false;
    }
    bytes.as_chunks::<2>().0.iter().enumerate().all(|(index, bytes)| {
        let index = offset + index;
        let position = u32::try_from(index % 1920).unwrap_or(1920);
        let edge = position.min(1919 - position).min(240);
        let fade = (1.0 - (std::f64::consts::PI * f64::from(edge) / 240.0).cos()) / 2.0;
        let frequency = if index < 1920 { 750.0 } else { 1000.0 };
        let expected =
            (983.0 * fade * (std::f64::consts::TAU * frequency * f64::from(position) / 24000.0).sin()).round();
        (f64::from(i16::from_le_bytes(*bytes)) - expected).abs() <= 1.0
    })
}

#[cfg(test)]
#[path = "playback_tests.rs"]
pub(super) mod tests;
