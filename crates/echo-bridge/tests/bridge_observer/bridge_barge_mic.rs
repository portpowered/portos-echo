//! T3: speech at the Echo's microphone under a reply, on a scripted
//! timeline. The Live session's transcript of it comes 700 ms after the
//! speech begins; the reply must not play on until then.
use super::bridge_barge::{Model, said, stream};
use super::*;
use echo_bridge::{
    bridge_codex::{Event, MediaEvent},
    bridge_voice::{Notice, Options, ReplyQuality, Stats, run},
};
use echo_link::{EchoLink, LinkConnector, LinkEvent, LinkEvents};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI16, AtomicUsize, Ordering},
};
use tokio::{sync::mpsc, time::Instant};

const FRAME: Duration = Duration::from_millis(20);
/// hassmic sends 1024 samples at 16 kHz in each microphone chunk.
const CHUNK: Duration = Duration::from_millis(64);
/// Reply levels, and the level of a voice near the microphone.
const TALK: i16 = 2000;
const LOUD: i16 = 6000;
const VOICE: i16 = 6000;
/// The Live session's first transcript fragment after speech begins. An
/// assumption of this timeline, not a measurement.
const TRANSCRIPT_LAG: Duration = Duration::from_millis(700);

/// The room the loopback device stands in.
#[derive(Default)]
struct Room {
    /// Amplitude of somebody speaking near the microphone.
    voice: AtomicI16,
    /// Each reply chunk the device was sent: when, and its loudest sample.
    played: Mutex<Vec<(Instant, u16)>>,
    flushed: Mutex<Option<Instant>>,
    /// When the voice began.
    onset: Mutex<Option<Instant>>,
}

impl Room {
    fn speak(&self, level: i16) {
        self.voice.store(level, Ordering::SeqCst);
        if level > 0 {
            *self.onset.lock().unwrap() = Some(Instant::now());
        }
    }

    /// One microphone chunk: a tenth of what the speaker plays now, as the
    /// echo canceller leaves it, and the voice. A 400 Hz tone at that level.
    fn microphone(&self) -> Vec<u8> {
        let last = self.played.lock().unwrap().last().copied();
        let playing = last.filter(|(at, _)| at.elapsed() < FRAME * 5);
        let echo = playing.map_or(0, |(_, level)| level / 10);
        let level = f32::from(echo) + f32::from(self.voice.load(Ordering::SeqCst));
        (0..1024u16)
            .flat_map(|index| {
                let phase = f32::from(index) * std::f32::consts::TAU * 400.0 / 16000.0;
                #[expect(clippy::cast_possible_truncation, reason = "the level is within i16")]
                let sample = (level * phase.sin()) as i16;
                sample.to_le_bytes()
            })
            .collect()
    }

    /// From the voice's first sound to the device having played out all the
    /// reply audio it was sent: frames play back to back from the first.
    fn onset_to_silence(&self) -> Duration {
        let onset = self.onset.lock().unwrap().unwrap();
        let flushed = *self.flushed.lock().unwrap();
        let (mut clock, mut end) = (None, onset);
        for (at, level) in self.played.lock().unwrap().iter() {
            if flushed.is_some_and(|flushed| *at >= flushed) {
                break;
            }
            let start = clock.map_or(*at, |clock: Instant| clock.max(*at));
            clock = Some(start + FRAME);
            if *level >= 1000 {
                end = start + FRAME;
            }
        }
        let end = flushed.map_or(end, |flushed| end.min(flushed));
        end.saturating_duration_since(onset)
    }
}

type Reply = (&'static str, Value);

/// hassmic under `-D`, with a microphone: it wakes once registered, sends a
/// chunk of the room every 64 ms and reports `played` for a finished reply
/// and for a flush.
async fn device(listener: tokio::net::TcpListener, room: Arc<Room>) -> mpsc::UnboundedSender<Reply> {
    let (mut reader, mut writer) = listener.accept().await.unwrap().0.into_split();
    let (replies, mut queued) = mpsc::unbounded_channel::<Reply>();
    let heard = room.clone();
    tokio::spawn(async move {
        let mut capturing = false;
        let mut tick = tokio::time::interval(CHUNK);
        loop {
            tokio::select! {
                reply = queued.recv() => {
                    let Some((kind, data)) = reply else { return };
                    capturing |= kind == "run-pipeline";
                    bridge_short::send(&mut writer, kind, data).await;
                }
                _ = tick.tick(), if capturing => {
                    let event = echo_contracts::wyoming::Event {
                        header: echo_contracts::wyoming::Header {
                            r#type: "audio-chunk".into(),
                            data: Some(json!({"rate":16000,"width":2,"channels":1,"timestamp":0})),
                            data_length: 0,
                            payload_length: 0,
                        },
                        data: None,
                        payload: heard.microphone(),
                    };
                    // The bridge may have ended the run: nobody is listening.
                    let _ = writer.write_all(&echo_contracts::wyoming::encode(&event)).await;
                }
            }
        }
    });
    loop {
        let (header, payload) = bridge_short::read(&mut reader).await;
        match header["type"].as_str().unwrap() {
            "describe" => replies
                .send(("info", json!({"satellite":{"name":"synthetic"}})))
                .unwrap(),
            "run-satellite" => {
                let wake =
                    json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"});
                replies.send(("run-pipeline", wake)).unwrap();
            }
            "hassmic-flush" => {
                *room.flushed.lock().unwrap() = Some(Instant::now());
                replies.send(("played", json!({}))).unwrap();
            }
            "audio-stop" => replies.send(("played", json!({}))).unwrap(),
            "audio-chunk" => {
                let (samples, _) = payload.as_chunks::<2>();
                let loudest = samples.iter().map(|s| i16::from_le_bytes(*s).unsigned_abs()).max();
                room.played.lock().unwrap().push((Instant::now(), loudest.unwrap_or(0)));
            }
            "hassmic-end" => return replies,
            _ => (),
        }
    }
}

/// Run one call against the room. `script` is the Live model's side.
async fn call(script: impl AsyncFnOnce(&mpsc::Sender<MediaEvent>, &Room)) -> (Arc<Room>, Stats, Vec<ReplyQuality>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let room = Arc::new(Room::default());
    let hassmic = tokio::spawn(device(listener, room.clone()));
    let connector = echo_wyoming::adapter::connector(address, echo_wyoming::ClientOptions::default());
    let mut client = connector.connect().await.unwrap();
    let mut events = client.events().unwrap();
    client.enable_capture(30).await.unwrap();
    let Some(LinkEvent::Wake(microphone)) = events.recv().await.unwrap() else {
        unreachable!("the device wakes first")
    };
    let (sender, receiver) = mpsc::channel(256);
    let mut port = Model {
        events: receiver,
        retired: Arc::new(AtomicUsize::new(0)),
    };
    let settled = std::cell::RefCell::new(Vec::new());
    let options = Options {
        listen_timeout: Some(Duration::from_secs(1)),
        session_max: Duration::from_secs(600),
        reply_lead_ms: 200,
        response_deadline: Instant::now() + Duration::from_secs(10),
        played_timeout: Duration::from_secs(2),
        muted: false,
    };
    let work = async {
        let observe = |notice| {
            if let Notice::ReplySettled(stats) = notice {
                settled.borrow_mut().push(stats.quality);
            }
            Ok(())
        };
        let pending = std::future::pending();
        let result = run(&client, &mut events, microphone, &mut port, options, pending, observe).await;
        drop(port);
        result
    };
    let model = async {
        script(&sender, &room).await;
        // Keep the call open while the run listens out its idle window.
        sender.closed().await;
    };
    let (result, ()) = tokio::join!(work, model);
    let _device = hassmic.await.unwrap();
    client.close().await.unwrap();
    (room, result.unwrap(), settled.into_inner())
}

/// The owner talks over a long reply. The model talks on for a second, as
/// it does until the session has heard the user, and then yields.
async fn talked_over(model: &mpsc::Sender<MediaEvent>, room: &Room) {
    model.send(said("assistant", "A long answer", false)).await.unwrap();
    stream(model, 75, TALK).await;
    room.speak(VOICE);
    stream(model, 35, TALK).await;
    model.send(said("user", "wait", false)).await.unwrap();
    stream(model, 15, TALK).await;
    stream(model, 40, 0).await;
    room.speak(0);
    model.send(said("user", "wait, stop", true)).await.unwrap();
    model.send(said("assistant", "New answer", false)).await.unwrap();
    stream(model, 5, 12000).await;
    let text = "New answer".into();
    model
        .send(MediaEvent::Control(Event::ReplyDone { text }))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 real Wyoming client and loopback device"]
async fn barge_in_silences_the_reply_before_the_transcript_of_the_speech() {
    let (room, stats, settled) = tokio::time::timeout(OBSERVATION_TIMEOUT, Box::pin(call(talked_over)))
        .await
        .unwrap();
    let silence = room.onset_to_silence();
    writeln!(
        std::io::stdout(),
        "BARGE onset_to_silence_ms={} transcript_lag_ms={}",
        silence.as_millis(),
        TRANSCRIPT_LAG.as_millis()
    )
    .unwrap();
    // The device goes quiet on the speech itself, not 700 ms later on its
    // transcript: detection, and the 200 ms the device already holds.
    assert!(silence + FRAME * 5 < TRANSCRIPT_LAG, "{silence:?}");
    let cuts: Vec<_> = settled.iter().map(|quality| quality.cuts).collect();
    assert_eq!((cuts, stats.reply_dropped), (vec![1, 0], 0));
}

/// The reply gets three times louder and so does its echo; then a short
/// loud noise that the session transcribes as nothing, while the model
/// talks on.
async fn noisy(model: &mpsc::Sender<MediaEvent>, room: &Room) {
    model.send(said("assistant", "A long answer", false)).await.unwrap();
    stream(model, 50, TALK).await;
    stream(model, 50, LOUD).await;
    room.speak(VOICE);
    stream(model, 10, TALK).await;
    room.speak(0);
    stream(model, 100, TALK).await;
    let text = "A long answer".into();
    model
        .send(MediaEvent::Control(Event::ReplyDone { text }))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 real Wyoming client and loopback device"]
async fn barge_in_echo_and_noise_at_the_microphone_lose_no_reply_audio() {
    let (room, stats, settled) = tokio::time::timeout(OBSERVATION_TIMEOUT, Box::pin(call(noisy)))
        .await
        .unwrap();
    // Nothing is cut or flushed, and every audible frame reaches the device.
    assert_eq!(*room.flushed.lock().unwrap(), None);
    let audible = room
        .played
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, level)| *level >= 1000)
        .count();
    let quality = settled[0];
    writeln!(std::io::stdout(), "BARGE noise: audible={audible} {quality:?}").unwrap();
    // The 210 frames the model spoke, and the fades around a hold.
    assert!(audible >= 210 && stats.reply_dropped == 0, "{audible} {stats:?}");
    // The louder reply held nothing; the noise held the reply at most once.
    assert_eq!((settled.len(), quality.cuts, quality.discarded_audible), (1, 0, 0));
    assert!(quality.holds <= 1, "{quality:?}");
}
