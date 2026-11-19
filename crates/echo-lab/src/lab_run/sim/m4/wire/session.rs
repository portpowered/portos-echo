//! Post-write observations distinguish speaker streams from explicit run end.
//! Keep signed sample statistics only; no captured or reply PCM is retained.
use echo_contracts::wyoming::Event;
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Duration};

#[derive(Clone, Debug, Default)]
pub(super) struct Session {
    registrations: Vec<Vec<Value>>,
    opened: Vec<Duration>,
    ended: Vec<Duration>,
    capture_after_played: usize,
    unmatched_played: usize,
    streams: Vec<Playback>,
    active: Option<usize>,
    pending: VecDeque<usize>,
}

#[derive(Clone, Debug, Default)]
struct Playback {
    started: Option<Duration>,
    stopped: Option<Duration>,
    played: Option<Duration>,
    ending: Option<&'static str>,
    earcon: bool,
    bytes: usize,
    audible_samples: usize,
    minimum: Option<i16>,
    maximum: Option<i16>,
}

impl Session {
    pub fn generation(&mut self) {
        self.registrations.push(vec![]);
    }

    pub fn registrations_ready(&self) -> bool {
        self.registrations.len() == 2 && self.registrations[1].len() == 1
    }

    /// Completed reply streams the device has reported Played.
    fn played(&self) -> usize {
        self.streams
            .iter()
            .filter(|stream| stream.played.is_some() && stream.ending == Some("stop") && !stream.earcon)
            .count()
    }

    fn checkpoint(&self, replies: usize, ends: usize) -> Result<bool, String> {
        let played = self.played();
        if played > replies || self.ended.len() > ends {
            return Err("wire: session advanced beyond expected reply/run boundary".into());
        }
        Ok(played == replies && self.ended.len() == ends)
    }

    pub fn capture(&mut self, event: &Event, now: Duration) {
        match event.header.r#type.as_str() {
            "run-pipeline" => self.opened.push(now),
            "played" => {
                if let Some(index) = self.pending.pop_front() {
                    self.streams[index].played = Some(now);
                } else {
                    self.unmatched_played += 1;
                }
            }
            "audio-chunk"
                if self
                    .streams
                    .iter()
                    .any(|stream| stream.played.is_some() && stream.ending == Some("stop") && !stream.earcon) =>
            {
                self.capture_after_played += 1;
            }
            _ => (),
        }
    }

    pub fn playback(&mut self, event: &Event, now: Duration, earcon: bool) {
        match event.header.r#type.as_str() {
            "run-satellite" => {
                if let (Some(generation), Ok(echo_contracts::wyoming::WyomingEvent::RunSatellite(data))) =
                    (self.registrations.last_mut(), event.typed())
                {
                    generation.push(data);
                }
            }
            "audio-start" => {
                self.streams.push(Playback {
                    started: Some(now),
                    ..Playback::default()
                });
                self.active = Some(self.streams.len() - 1);
            }
            "audio-chunk" => {
                // A missing start stays missing in evidence; never synthesize it.
                let index = *self.active.get_or_insert_with(|| {
                    self.streams.push(Playback::default());
                    self.streams.len() - 1
                });
                let stream = &mut self.streams[index];
                stream.earcon |= earcon;
                stream.bytes += event.payload.len();
                for bytes in event.payload.as_chunks::<2>().0 {
                    let sample = i16::from_le_bytes(*bytes);
                    stream.minimum = Some(stream.minimum.map_or(sample, |old| old.min(sample)));
                    stream.maximum = Some(stream.maximum.map_or(sample, |old| old.max(sample)));
                    stream.audible_samples += usize::from(sample.unsigned_abs() > 64);
                }
            }
            "audio-stop" => self.stop(now, "stop"),
            "hassmic-flush" => self.stop(now, "flush"),
            "hassmic-end" => self.ended.push(now),
            _ => (),
        }
    }

    fn stop(&mut self, now: Duration, ending: &'static str) {
        if let Some(index) = self.active.take() {
            self.streams[index].stopped = Some(now);
            self.streams[index].ending = Some(ending);
            self.pending.push_back(index);
        }
    }

    pub fn observation(&self) -> Value {
        let times = |values: &[Duration]| values.iter().map(Duration::as_micros).collect::<Vec<_>>();
        let streams = self
            .streams
            .iter()
            .map(|stream| {
                json!({
                    "started_us":stream.started.map(|time| time.as_micros()),
                    "stopped_us":stream.stopped.map(|time| time.as_micros()),
                    "played_us":stream.played.map(|time| time.as_micros()),
                    "ending":stream.ending,"earcon":stream.earcon,"pcm_bytes":stream.bytes,
                    "audible_samples":stream.audible_samples,"minimum":stream.minimum,"maximum":stream.maximum,
                })
            })
            .collect::<Vec<_>>();
        json!({"registrations":self.registrations,"run_opened_us":times(&self.opened),"explicit_end_us":times(&self.ended),
            "capture_chunks_after_reply_played":self.capture_after_played,
            "unmatched_played":self.unmatched_played,"streams":streams})
    }
}

impl super::Timing {
    /// Forwarded run, reply and capture boundaries for persistent journeys.
    pub fn session_observation(&self) -> Value {
        self.session.observation()
    }

    /// Replies the device reported Played. Until then its playout runs on the
    /// wall clock, so a journey must not race the bridge's clock past it.
    pub fn replies_played(&self) -> usize {
        self.session.played()
    }
}

impl super::Relay {
    /// Wait for forwarded device receipts, independently of bridge projections.
    /// An early end or an extra reply fails rather than satisfying a stale gate.
    /// The journey owner supplies its existing cancellation/deadline ceiling.
    pub async fn wait_session(&self, replies: usize, ends: usize) -> Result<Value, String> {
        let mut observed = self.observed.clone();
        loop {
            let timing = observed.borrow_and_update().clone();
            if let Some(error) = timing.failure {
                return Err(error);
            }
            if timing.session.checkpoint(replies, ends)? {
                return Ok(timing.session.observation());
            }
            observed
                .changed()
                .await
                .map_err(|_| "wire: observer ended before session boundary")?;
        }
    }
}

#[cfg(test)]
mod tests;
