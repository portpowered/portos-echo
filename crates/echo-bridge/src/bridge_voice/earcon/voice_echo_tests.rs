//! T2: a reply through the run's own source with the Echo's speaker fed back
//! into its microphone: what goes upstream, and what `reply.quality` says.
//! A made-up voice and room on paused time; no file, device or network.
use super::super::*;
use echo_audio::echo_room::{echo, hiss, level_db, mix, voice};
use std::io::Write;

const SECOND: usize = 16_000;
/// The Echo's speaker-to-microphone delay as the bridge sees it (baselines).
const DELAY: usize = 5120;

/// A media sender that keeps every frame it is given.
#[derive(Default)]
struct Uplink(std::cell::RefCell<Vec<i16>>);

impl VoicePort for Uplink {
    fn send(&self, frame: PcmFrame) -> Result<(), Error> {
        self.0.borrow_mut().extend_from_slice(&frame);
        Ok(())
    }
    async fn next(&mut self) -> Result<crate::bridge_codex::MediaEvent, Error> {
        std::future::pending().await
    }
    fn ready_audio(&mut self) -> impl Future<Output = Result<Option<PcmFrame>, Error>> {
        std::future::ready(Ok(None))
    }
}

/// The Echo and its room: frames written play back to back from the first,
/// and the microphone hears them 320 ms later, half as loud, with the
/// room's reflections and floor. Nobody speaks.
struct Room {
    resampler: echo_audio::PcmResampler,
    played: Vec<i16>,
    floor: Vec<i16>,
    heard: usize,
}

impl Room {
    fn new(seconds: usize) -> Self {
        let written = echo_contracts::wyoming::AudioFormat {
            rate: 24_000,
            width: 2,
            channels: 1,
            extra: std::collections::BTreeMap::default(),
        };
        Self {
            resampler: echo_audio::PcmResampler::new(&written, 16_000).unwrap(),
            played: Vec::new(),
            floor: hiss(7, seconds * SECOND, 12.0),
            heard: 0,
        }
    }

    fn play(&mut self, frame: &[u8]) {
        let (samples, _) = frame.as_chunks::<2>();
        let samples: Vec<i16> = samples.iter().map(|sample| i16::from_le_bytes(*sample)).collect();
        let mut scratch = vec![0; self.resampler.output_capacity()];
        let progress = self.resampler.process(&samples, &mut scratch).unwrap();
        self.played.extend_from_slice(&scratch[..progress.written]);
    }

    /// The next 20 ms at the microphone, as hassmic sends it.
    fn microphone(&mut self) -> MicFrame {
        let end = self.heard + 320;
        // The 16 ms of reflections reach back 256 samples before the frame.
        let mut source = vec![0; 576];
        for (offset, sample) in source.iter_mut().enumerate() {
            let index = (end + offset).checked_sub(DELAY + 576);
            *sample = index.and_then(|index| self.played.get(index)).copied().unwrap_or(0);
        }
        let heard = mix(&echo(&source, 0, 0.5, 9)[256..], &self.floor[self.heard..end]);
        self.heard = end;
        MicFrame {
            format: crate::bridge_audio::format(16_000),
            payload: heard.iter().flat_map(|sample| sample.to_le_bytes()).collect(),
        }
    }
}

/// Eight seconds of reply with only its own echo in the room. Returns the
/// microphone, the uplink and the settled reply.
async fn reply(echo_delay_ms: Option<u16>) -> (Vec<i16>, Vec<i16>, Stats) {
    let mut source = Source::new(200, false, echo_delay_ms).unwrap();
    let (port, mut room) = (Uplink::default(), Room::new(8));
    let mut microphone = Vec::new();
    source.spoke();
    for said in voice(31, 8 * SECOND, 3000.0).as_chunks::<320>().0 {
        source.audio(said).unwrap();
        if source.playable()
            && let Ok(Some(frame)) = tokio::time::timeout(Duration::ZERO, source.reply.next_or_fade()).await
        {
            source.wrote(&frame);
            room.play(&frame);
        }
        tokio::time::advance(Duration::from_millis(20)).await;
        if room.played.is_empty() {
            continue;
        }
        let heard = room.microphone();
        let (samples, _) = heard.payload.as_chunks::<2>();
        microphone.extend(samples.iter().map(|sample| i16::from_le_bytes(*sample)));
        source.capture(Some(heard), &port).unwrap();
    }
    let mut stats = source.stats.clone();
    stats.quality = source.settle(0);
    (microphone, port.0.into_inner(), stats)
}

/// The `reply.quality` row the view writes for `stats`.
async fn quality_row(stats: Stats) -> serde_json::Value {
    use crate::bridge_voice::{ServeNotice, SessionNotice, voice_view::View};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = crate::bridge_trace::Trace::open(&path, "chatgpt").unwrap();
    let health = crate::bridge_health::Health::offline();
    let screen = crate::bridge_runtime::bridge_screen::Screen::bind(None).await.unwrap();
    let mut view = View::new(&screen, &health, &mut trace);
    view.observe(ServeNotice::Listening).unwrap();
    view.observe(ServeNotice::Session(SessionNotice::ReplyCompleted(stats)))
        .unwrap();
    let rows = std::fs::read_to_string(&path).unwrap();
    let mut rows = rows
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap());
    rows.find(|row| row["event"] == "reply.quality").unwrap()
}

/// (e) During a reply with nobody speaking, what goes upstream is close to
/// silence: at least 20 dB under the microphone from two seconds in. The
/// reply's `reply.quality` row carries what the canceller measured.
#[tokio::test(start_paused = true)]
async fn echo_cancel_leaves_a_reply_with_nobody_speaking_near_silent_upstream() {
    let (microphone, uplink, stats) = reply(Some(240)).await;
    assert_eq!(uplink.len(), microphone.len());
    let (before, after) = (level_db(&microphone[2 * SECOND..]), level_db(&uplink[2 * SECOND..]));
    let aec = stats.quality.aec.unwrap();
    let row = quality_row(stats.clone()).await;
    writeln!(
        std::io::stdout(),
        "source_echo_only microphone_db={before:.1} uplink_db={after:.1} erle_after_2s_db={:.1} aec={}",
        before - after,
        row["aec"]
    )
    .unwrap();
    // The microphone hears the reply at about 1,500 RMS; under 40 goes up.
    assert!(before >= 60.0 && after <= 32.0, "{before} {after}");
    assert!(before - after >= 20.0, "{before} {after}");
    assert!((296..=328).contains(&aec.delay_ms), "{aec:?}");
    assert!(aec.converged && aec.erle_db10 >= 200, "{aec:?}");
    // The ear still measures the microphone itself, not what was cleaned.
    assert!(stats.quality.mic.level_max >= 1000, "{:?}", stats.quality.mic);
    // The row holds those numbers, not placeholders.
    let erle_db = f64::from(aec.erle_db10) / 10.0;
    let expected = serde_json::json!({"delay_ms":aec.delay_ms, "erle_db":erle_db, "converged":true});
    assert_eq!(row["aec"], expected);
    assert!(row["aec"]["erle_db"].as_f64().unwrap() >= 20.0);
}

/// (d) `echo_cancel = false`: every microphone sample goes upstream as it
/// came, and the row says the canceller is off.
#[tokio::test(start_paused = true)]
async fn echo_cancel_switched_off_sends_the_microphone_byte_for_byte() {
    let (microphone, uplink, stats) = reply(None).await;
    assert!(level_db(&microphone[2 * SECOND..]) >= 60.0);
    assert_eq!(uplink, microphone);
    assert_eq!(stats.quality.aec, None);
    assert_eq!(quality_row(stats).await["aec"], serde_json::Value::Null);
}
