//! A passive measurement of the microphone under a reply, for `reply.quality`.
//! It decides nothing: the Live session alone judges a barge-in (`Barge`).
use std::collections::VecDeque;
use std::time::Duration;

const PERIOD: Duration = Duration::from_millis(20);
/// How long a frame written to the Echo may still sound at its microphone:
/// the speaker's output delay (about 85 ms, `Mouth`), the capture path back
/// and the room's tail.
const REACH: Duration = Duration::from_millis(700);
/// A level this far above the echo and the room is somebody speaking.
const RATIO: u32 = 3;
/// Microphone RMS below this is never speech, however quiet the room.
const MIN_LEVEL: u32 = 250;
/// Reply audio quieter than this is not playing, as far as the ear goes.
const REFERENCE_AUDIBLE: u32 = 300;
/// Reply audio quieter than this teaches nothing about the echo: against a
/// quiet frame the room's own noise, which the Echo amplifies by up to 35 dB,
/// reads as several times the reply level.
const REFERENCE_LOUD: u32 = 1500;
/// Loud frames, less quiet ones, before a sound counts as speech: 80 ms.
const NEED: u8 = 4;
/// Microphone frames heard under audible reply audio before the echo is known.
const WARMUP: u32 = 25;

/// What the microphone heard under one reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Heard {
    pub onsets: u32,
    pub echo_max: u32,
    pub level_max: u32,
    pub coupling: u32,
    pub echo_mean: u32,
    pub noise: u32,
}

/// A double-talk detector after Geigel: the microphone holds speech when it
/// is louder than the loudest reply audio recently played could make it.
/// The Echo's front end cancels most of its own speaker, and how much is
/// left differs by device, volume and room, so the ear learns it: the most
/// microphone level per reply level it has heard, taken freely during the
/// first half second of reply audio and afterwards only from frames it did
/// not take for speech, and then by at most 5% a frame. On the Echo the
/// microphone path cancels nothing and the reply is the loudest thing it
/// hears, so the ear holds and cuts nothing: it reports what it measured.
#[derive(Default)]
pub(crate) struct Ear {
    /// Start of playout and RMS of each frame recently written to the Echo.
    played: VecDeque<(Duration, u32)>,
    clock: Option<Duration>,
    /// Echo per thousand of reference, and the frames it was learned from.
    coupling: u32,
    learned: u32,
    /// The room with the speaker silent: falls at once, rises 2% a frame.
    noise: u32,
    run: u8,
    speaking: bool,
    /// Audible reply audio has been written since `rest`.
    voiced: bool,
    /// Any reply audio has.
    sounded: bool,
    heard: Heard,
    /// Sum and count of the ratios learned from in this reply.
    ratios: (u64, u32),
}

fn per_mille(level: u32, reference: u32) -> u32 {
    u32::try_from(u64::from(level) * 1000 / u64::from(reference.max(1))).unwrap_or(u32::MAX)
}

fn rms(samples: impl Iterator<Item = i16>) -> u32 {
    let (sum, count) = samples.fold((0u64, 0u64), |(sum, count), sample| {
        let sample = u64::from(sample.unsigned_abs());
        (sum + sample * sample, count + 1)
    });
    u32::try_from((sum / count.max(1)).isqrt()).unwrap_or(u32::MAX)
}

impl Ear {
    /// One signed 16-bit little-endian frame was written to the Echo at `now`.
    pub(crate) fn played(&mut self, frame: &[u8], now: Duration) {
        let (samples, _) = frame.as_chunks::<2>();
        let level = rms(samples.iter().map(|sample| i16::from_le_bytes(*sample)));
        let start = self.clock.filter(|end| *end > now).unwrap_or(now);
        self.clock = Some(start + PERIOD);
        self.voiced |= level >= REFERENCE_AUDIBLE;
        self.sounded = true;
        self.played.push_back((start, level));
        while self.played.front().is_some_and(|(at, _)| *at + REACH < now) {
            self.played.pop_front();
        }
    }

    /// One microphone frame arrived at `now`. True while somebody is speaking
    /// over a reply that has been audible.
    pub(crate) fn heard(&mut self, frame: &[i16], now: Duration) -> bool {
        let level = rms(frame.iter().copied());
        let reference = self.reference(now);
        let echo = u32::try_from(u64::from(reference) * u64::from(self.coupling) / 1000).unwrap_or(u32::MAX);
        let warm = self.learned >= WARMUP;
        let limit = MIN_LEVEL.max(echo.max(self.noise).saturating_mul(RATIO));
        let loud = level > limit && (warm || reference < REFERENCE_AUDIBLE);
        self.heard.level_max = self.heard.level_max.max(level);
        if loud {
            self.run = (self.run + 1).min(NEED);
        } else {
            self.run = self.run.saturating_sub(1);
            self.learn(level, reference, warm);
        }
        let speaking = self.voiced && if self.speaking { self.run > 0 } else { self.run == NEED };
        if speaking && !self.speaking {
            self.heard.onsets += 1;
        }
        self.speaking = speaking;
        speaking
    }

    /// The loudest reply frame that may sound at the microphone at `now`.
    fn reference(&self, now: Duration) -> u32 {
        let recent = self.played.iter().filter(|(at, _)| *at <= now && *at + REACH >= now);
        recent.map(|(_, level)| *level).max().unwrap_or(0)
    }

    /// A frame that was not speech says how loud the echo or the room is.
    fn learn(&mut self, level: u32, reference: u32, warm: bool) {
        if reference < REFERENCE_AUDIBLE {
            self.noise = if level < self.noise {
                level
            } else {
                self.noise + self.noise / 50 + 1
            };
            return;
        }
        self.heard.echo_max = self.heard.echo_max.max(level);
        if reference < REFERENCE_LOUD {
            return;
        }
        let ratio = per_mille(level.saturating_sub(self.noise), reference);
        self.ratios = (self.ratios.0 + u64::from(ratio), self.ratios.1 + 1);
        let most = if warm {
            self.coupling + self.coupling / 20 + 1
        } else {
            ratio
        };
        self.coupling = self.coupling.max(ratio.min(most));
        self.learned = self.learned.saturating_add(1);
    }

    /// Reply audio has been written to the Echo since `rest`.
    pub(crate) fn sounded(&self) -> bool {
        self.sounded
    }

    /// The reply ended or was cut: the next one starts unheard. What was
    /// learned of the echo and the room is kept for the call.
    pub(crate) fn rest(&mut self) -> Heard {
        self.voiced = false;
        self.sounded = false;
        self.speaking = false;
        self.run = 0;
        let heard = self.measurement();
        self.heard = Heard::default();
        self.ratios = (0, 0);
        heard
    }

    pub(crate) fn measurement(&self) -> Heard {
        let mut heard = self.heard;
        let (sum, count) = self.ratios;
        heard.echo_mean = u32::try_from(sum / u64::from(count.max(1))).unwrap_or(u32::MAX);
        heard.noise = self.noise;
        heard.coupling = self.coupling;
        heard
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(level: i16) -> Vec<u8> {
        std::iter::repeat_n(level.to_le_bytes(), 480).flatten().collect()
    }

    /// The Echo plays `reply` while the microphone hears a tenth of it plus
    /// `voice`, frame by frame. Returns the frames at which speech was heard.
    fn listen(ear: &mut Ear, now: &mut Duration, frames: u32, reply: i16, voice: i16) -> Vec<bool> {
        let mut heard = Vec::new();
        for _ in 0..frames {
            ear.played(&pcm(reply), *now);
            heard.push(ear.heard(&[reply / 10 + voice; 320], *now));
            *now += PERIOD;
        }
        heard
    }

    /// T1: the reply's own echo, however loud the reply, is never speech; a
    /// voice over it is heard within 80 ms and for as long as it lasts.
    #[test]
    fn ear_hears_speech_over_the_reply_but_not_the_reply() {
        let (mut ear, mut now) = (Ear::default(), Duration::ZERO);
        assert!(!listen(&mut ear, &mut now, 100, 8000, 0).contains(&true));
        // The reply gets louder: its echo does too, and is still not speech.
        assert!(!listen(&mut ear, &mut now, 100, 24000, 0).contains(&true));
        assert!(!listen(&mut ear, &mut now, 50, 8000, 0).contains(&true));
        let heard = listen(&mut ear, &mut now, 10, 8000, 6000);
        assert_eq!(heard.iter().position(|speech| *speech), Some(3));
        assert!(heard[3..].iter().all(|speech| *speech));
        // It stops within 80 ms of the voice, and the echo is not mistaken.
        let after = listen(&mut ear, &mut now, 50, 8000, 0);
        assert_eq!(after.iter().rposition(|speech| *speech), Some(2));
        let measured = ear.rest();
        assert_eq!((measured.onsets, measured.coupling), (1, 100));
        assert_eq!((measured.echo_max, measured.level_max), (2400, 6800));
    }

    /// T1: before any reply audio the ear is not asked; once the echo is
    /// known, speech into a pause of the reply is heard against the room.
    #[test]
    fn ear_waits_for_reply_audio_and_hears_speech_in_its_pauses() {
        let (mut ear, mut now) = (Ear::default(), Duration::ZERO);
        // The user finishes the request: no reply has played, nothing to cut.
        for _ in 0..50 {
            assert!(!ear.heard(&[5000; 320], now));
            now += PERIOD;
        }
        // Speech in the first half second of reply audio is not told from
        // echo yet; it only makes the ear harder of hearing.
        assert!(!listen(&mut ear, &mut now, 20, 8000, 6000).contains(&true));
        assert!(!listen(&mut ear, &mut now, 30, 8000, 0).contains(&true));
        // The model pauses; its tail dies away, then the user speaks.
        assert!(!listen(&mut ear, &mut now, 60, 0, 40).contains(&true));
        let heard = listen(&mut ear, &mut now, 10, 0, 3000);
        assert_eq!(heard.iter().position(|speech| *speech), Some(3));
        // The next reply starts unheard and keeps what was learned.
        assert_eq!(ear.rest().coupling, 850);
        assert!(!ear.heard(&[5000; 320], now));
    }

    /// T1, the device's deaf ear: under quiet reply audio the room's noise
    /// read as an echo several times the reply, and no voice could top it.
    #[test]
    fn ear_learns_nothing_of_the_echo_from_quiet_reply_audio() {
        let (mut ear, mut now) = (Ear::default(), Duration::ZERO);
        for _ in 0..50 {
            ear.played(&pcm(400), now);
            assert!(!ear.heard(&[1200; 320], now));
            now += PERIOD;
        }
        assert!(!listen(&mut ear, &mut now, 100, 8000, 0).contains(&true));
        let heard = listen(&mut ear, &mut now, 10, 8000, 6000);
        assert_eq!(heard.iter().position(|speech| *speech), Some(3));
        let measured = ear.rest();
        assert_eq!((measured.coupling, measured.echo_mean), (100, 100));
    }

    /// T1: a slow rise in level under the reply is the echo changing, such
    /// as the canceller losing its grip, not somebody speaking.
    #[test]
    fn ear_follows_an_echo_that_grows_slowly() {
        let (mut ear, mut now) = (Ear::default(), Duration::ZERO);
        listen(&mut ear, &mut now, 50, 8000, 0);
        for step in 0..100 {
            assert!(!listen(&mut ear, &mut now, 1, 8000, step * 16).contains(&true));
        }
        assert!(ear.rest().coupling > 250);
    }
}
