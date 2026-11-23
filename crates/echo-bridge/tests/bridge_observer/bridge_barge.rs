//! T3: the run owner over the real Wyoming client and a loopback device, with
//! a scripted Live model. The device's barge-in: the owner talked over a long
//! reply and it played on, then the run died with no reply rows.
use super::*;
use echo_bridge::{
    Error,
    bridge_codex::{Event, MediaEvent},
    bridge_voice::{Notice, Options, VoicePort, run},
};
use echo_link::{EchoLink, LinkConnector, LinkEvent, LinkEvents};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::mpsc;

const FRAME: Duration = Duration::from_millis(20);
/// The cut reply's level, and the level of the answer to the interruption.
const OLD: i16 = 2000;
const NEW: i16 = 12000;

/// The Live model's side of the call. The pinned V3 session takes no cancel
/// request: retiring the receive side is all the bridge can do upstream.
pub(super) struct Model {
    pub(super) events: mpsc::Receiver<MediaEvent>,
    pub(super) retired: Arc<AtomicUsize>,
}
impl VoicePort for Model {
    fn send(&self, _: echo_media::PcmFrame) -> Result<(), Error> {
        Ok(())
    }
    async fn next(&mut self) -> Result<MediaEvent, Error> {
        self.events.recv().await.ok_or(Error::Unavailable)
    }
    fn ready_audio(&mut self) -> impl Future<Output = Result<Option<echo_media::PcmFrame>, Error>> {
        std::future::ready(Ok(None))
    }
    fn retire_audio(&mut self) {
        self.retired.fetch_add(1, Ordering::SeqCst);
    }
}

/// What the device was sent, in order: the event type and, for reply audio,
/// the loudest sample of the chunk.
type Wire = Arc<Mutex<Vec<(String, u16)>>>;

/// hassmic under `-D`: wakes once registered, answers probes, and reports
/// `played` for a finished reply and for a flush.
async fn device(listener: tokio::net::TcpListener, wire: Wire) -> tokio::net::TcpStream {
    let mut stream = listener.accept().await.unwrap().0;
    loop {
        let (header, payload) = bridge_short::read(&mut stream).await;
        let kind = header["type"].as_str().unwrap().to_owned();
        match kind.as_str() {
            "describe" => {
                bridge_short::send(&mut stream, "info", json!({"satellite":{"name":"synthetic"}})).await;
                continue;
            }
            "run-satellite" => {
                let row: Value = serde_json::from_str(
                    include_str!("../../../../tests/fixtures/wyoming/wake-word.jsonl")
                        .lines()
                        .nth(1)
                        .unwrap(),
                )
                .unwrap();
                bridge_short::send(&mut stream, "run-pipeline", row["header"]["data"].clone()).await;
            }
            "audio-stop" | "hassmic-flush" => bridge_short::send(&mut stream, "played", json!({})).await,
            _ => (),
        }
        let (samples, _) = payload.as_chunks::<2>();
        let loudest = samples.iter().map(|s| i16::from_le_bytes(*s).unsigned_abs()).max();
        wire.lock().unwrap().push((kind.clone(), loudest.unwrap_or(0)));
        if kind == "hassmic-end" {
            return stream;
        }
    }
}

pub(super) fn said(role: &str, text: &str, done: bool) -> MediaEvent {
    MediaEvent::control(Event::Transcript {
        role: role.into(),
        text: text.into(),
        done,
    })
}

pub(super) async fn stream(model: &mpsc::Sender<MediaEvent>, count: usize, level: i16) {
    for _ in 0..count {
        model.send(MediaEvent::audio([level; 320])).await.unwrap();
        tokio::time::sleep(FRAME).await;
    }
}

fn count(wire: &Wire, kind: &str) -> usize {
    wire.lock().unwrap().iter().filter(|(k, _)| k == kind).count()
}

/// The model's script. Returns how many audible frames of the first reply it
/// had sent when the user began to talk over it.
async fn model(model: mpsc::Sender<MediaEvent>, wire: Wire) -> usize {
    interrupted(&model, &wire).await;
    answered(&model, &wire).await;
    // Keep the call open while the run listens out its idle window.
    model.closed().await;
    90
}

/// A long reply, and the user talking over it until the model yields.
async fn interrupted(model: &mpsc::Sender<MediaEvent>, wire: &Wire) {
    // The question's transcript is whole: a later one is the user talking over.
    model.send(said("user", "tell me a story", true)).await.unwrap();
    model.send(said("assistant", "A long answer", false)).await.unwrap();
    // Faster than real time: the bridge queues what the device is not owed yet.
    for _ in 0..60 {
        model.send(MediaEvent::audio([OLD; 320])).await.unwrap();
    }
    stream(model, 30, OLD).await;
    // The user talks over the reply. The model talks on for 600 ms, then yields.
    model.send(said("user", "wait", false)).await.unwrap();
    stream(model, 30, OLD).await;
    stream(model, 40, 0).await;
    assert_eq!(count(wire, "hassmic-flush"), 1);
}

/// Late audio of the cut response, then the model's answer to the user.
async fn answered(model: &mpsc::Sender<MediaEvent>, wire: &Wire) {
    stream(model, 10, OLD).await;
    model.send(said("user", "wait, stop", true)).await.unwrap();
    stream(model, 10, OLD).await;
    // Nothing was opened for the cut response; its answer opens the next reply.
    assert_eq!(count(wire, "audio-start"), 1);
    model.send(said("assistant", "New answer", false)).await.unwrap();
    stream(model, 5, NEW).await;
    let text = "New answer".into();
    model
        .send(MediaEvent::control(Event::ReplyDone { text }))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "T3 real Wyoming client and loopback device"]
async fn barge_in_stops_reply_audio_on_the_wire_and_settles_the_cut_reply() {
    tokio::time::timeout(OBSERVATION_TIMEOUT, Box::pin(exercise()))
        .await
        .unwrap();
}

async fn exercise() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let wire = Wire::default();
    let hassmic = tokio::spawn(device(listener, wire.clone()));
    let connector = echo_wyoming::adapter::connector(address, echo_wyoming::ClientOptions::default());
    let mut client = connector.connect().await.unwrap();
    let mut events = client.events().unwrap();
    client.enable_capture(30).await.unwrap();
    let Some(LinkEvent::Wake(microphone)) = events.recv().await.unwrap() else {
        unreachable!("the device wakes first")
    };
    let (sender, receiver) = mpsc::channel(256);
    let retired = Arc::new(AtomicUsize::new(0));
    let mut port = Model {
        events: receiver,
        retired: retired.clone(),
    };
    let settled = std::cell::RefCell::new(Vec::new());
    let played = std::cell::Cell::new(0);
    let options = Options {
        listen_timeout: Some(Duration::from_secs(1)),
        session_max: Duration::from_secs(600),
        reply_lead_ms: 200,
        response_deadline: tokio::time::Instant::now() + Duration::from_secs(10),
        played_timeout: Duration::from_secs(2),
        muted: false,
    };
    let work = async {
        let result = run(
            &client,
            &mut events,
            microphone,
            &mut port,
            options,
            std::future::pending(),
            |notice| {
                match notice {
                    Notice::ReplySettled(stats) => settled.borrow_mut().push(stats.quality),
                    Notice::ReplyPlayed => played.set(played.get() + 1),
                    _ => (),
                }
                Ok(())
            },
        )
        .await;
        drop(port);
        result
    };
    let (result, before) = tokio::join!(work, model(sender, wire.clone()));
    let stats = result.unwrap();
    let _device = hassmic.await.unwrap();
    client.close().await.unwrap();
    let wire = wire.lock().unwrap().clone();
    let cuts: Vec<_> = settled.borrow().iter().map(|quality| quality.cuts).collect();
    heard(&wire, before, &cuts);
    // The cut reply and the answer each settle with their rows.
    assert_eq!((cuts, played.get()), (vec![1, 0], 2));
    assert_eq!(retired.load(Ordering::SeqCst), 1);
    assert_eq!(stats.reply_dropped, 0);
}

/// What the device was sent: one flush, nothing of the cut response after
/// it, and the answer whole in a playback of its own.
fn heard(wire: &[(String, u16)], before: usize, cuts: &[u32]) {
    let controls: Vec<_> = wire
        .iter()
        .map(|(kind, _)| kind.as_str())
        .filter(|kind| *kind != "audio-chunk")
        .collect();
    assert_eq!(
        controls,
        [
            "run-satellite",
            "audio-start",
            "hassmic-flush",
            "audio-start",
            "audio-stop",
            "hassmic-end"
        ]
    );
    let flush = wire.iter().position(|(kind, _)| kind == "hassmic-flush").unwrap();
    let chunks = |range: &[(String, u16)], from: u16, to: u16| {
        let chunk = |(kind, level): &&(String, u16)| kind == "audio-chunk" && (from..to).contains(level);
        range.iter().filter(chunk).count()
    };
    // Reply audio stopped at the user's first word: the 30 frames the model
    // spoke after it never reached the device, however the test was scheduled.
    let (old, late, new) = (
        chunks(&wire[..flush], 1000, 6000),
        chunks(&wire[flush..], 0, 6000),
        chunks(&wire[flush..], 6000, u16::MAX),
    );
    writeln!(
        std::io::stdout(),
        "BARGE wire: old_audible_before_flush={old} sent_before_barge={before} after_flush_old={late} new={new} cuts={cuts:?}"
    )
    .unwrap();
    assert!(
        old <= before + 1,
        "{} reply frames reached the device after the barge-in",
        old - before
    );
    // Nothing of the cut response plays after the flush; the answer plays whole.
    assert_eq!((late, new), (0, 5));
}
