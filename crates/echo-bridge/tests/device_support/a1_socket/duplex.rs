//! T3: the shipped device role under the fork's `-P wyoming -D -E`, reached
//! through the bridge's World over loopback TCP, and the production run owner
//! with a scripted Live model. A long turn: a quiet delegation, then a reply
//! longer than the device's failsafe, talked over by the user.
use super::*;
use crate::bridge_barge::said;
use echo_bridge::{
    Error,
    bridge_codex::{Event as Media, MediaEvent},
    bridge_voice::{Notice, Options, Stats, VoicePort, run},
};
use echo_link::{EchoLink, LinkEvent, LinkEvents};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::mpsc;

const FRAME: Duration = Duration::from_millis(20);
/// The device's failsafe for this run: the reply below is twice as long.
const FAILSAFE: u32 = 3;

/// The Live model's side. It counts the microphone frames sent upstream.
struct Model {
    events: mpsc::Receiver<MediaEvent>,
    upstream: Arc<AtomicUsize>,
}

impl VoicePort for Model {
    fn send(&self, _: echo_media::PcmFrame) -> Result<(), Error> {
        self.upstream.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn next(&mut self) -> Result<MediaEvent, Error> {
        self.events.recv().await.ok_or(Error::Unavailable)
    }
    fn ready_audio(&mut self) -> impl Future<Output = Result<Option<echo_media::PcmFrame>, Error>> {
        std::future::ready(Ok(None))
    }
    fn retire_audio(&mut self) {}
}

async fn speak(model: &mpsc::Sender<MediaEvent>, frames: usize, level: i16) {
    for _ in 0..frames {
        model.send(MediaEvent::Audio(Box::new([level; 320]))).await.unwrap();
        tokio::time::sleep(FRAME).await;
    }
}

/// Upstream microphone frames over one stretch of the script.
async fn counted(upstream: &AtomicUsize, stretch: impl Future<Output = ()>) -> usize {
    let before = upstream.load(Ordering::SeqCst);
    stretch.await;
    upstream.load(Ordering::SeqCst) - before
}

/// A web search: the question, a quiet delegation, then a long answer the user
/// talks over once the session has heard them. Returns the upstream frames
/// during the delegation, the first and second halves of the reply.
async fn long_turn(model: mpsc::Sender<MediaEvent>, upstream: Arc<AtomicUsize>) -> [usize; 3] {
    model.send(said("user", "search the web", true)).await.unwrap();
    let quiet = counted(&upstream, tokio::time::sleep(Duration::from_secs(2))).await;
    model.send(said("assistant", "A long answer", false)).await.unwrap();
    let first = counted(&upstream, speak(&model, 150, 2000)).await;
    let second = counted(&upstream, speak(&model, 150, 2000)).await;
    // Only microphone audio lets the session hear the user over the reply.
    if second > 0 {
        model.send(said("user", "stop", false)).await.unwrap();
        speak(&model, 30, 2000).await;
        speak(&model, 20, 0).await;
        model.send(said("user", "stop", true)).await.unwrap();
        model.send(said("assistant", "Stopped", false)).await.unwrap();
        speak(&model, 5, 9000).await;
    }
    let text = "Stopped".into();
    model
        .send(MediaEvent::Control(Media::ReplyDone { text }))
        .await
        .unwrap();
    model.closed().await;
    [quiet, first, second]
}

async fn call(satellite: &fixture::Satellite) -> (Result<Stats, Error>, Vec<Stats>, [usize; 3]) {
    let world = echo_bridge::world_witness::HelperWorld::new(TCP.parse().unwrap()).unwrap();
    let mut client = world.connect().await.unwrap();
    let mut events = EchoLink::events(&mut client).unwrap();
    client.enable_capture(FAILSAFE).await.unwrap();
    client.probe().await.unwrap();
    satellite.signal("-USR1").await;
    let microphone = loop {
        match events.recv().await.unwrap() {
            Some(LinkEvent::Wake(microphone)) => break microphone,
            Some(_) => (),
            None => unreachable!("the device wakes"),
        }
    };
    let (sender, receiver) = mpsc::channel(512);
    let upstream = Arc::new(AtomicUsize::new(0));
    let mut port = Model {
        events: receiver,
        upstream: upstream.clone(),
    };
    let settled = std::cell::RefCell::new(Vec::new());
    let options = Options {
        listen_timeout: Some(Duration::from_secs(1)),
        session_max: Duration::from_secs(600),
        reply_lead_ms: 200,
        response_deadline: tokio::time::Instant::now() + Duration::from_secs(20),
        played_timeout: Duration::from_secs(2),
        muted: false,
    };
    let work = async {
        let observe = |notice| {
            if let Notice::ReplySettled(stats) = notice {
                settled.borrow_mut().push(stats);
            }
            Ok(())
        };
        let result = run(
            &client,
            &mut events,
            microphone,
            &mut port,
            options,
            std::future::pending(),
            observe,
        )
        .await;
        drop(port);
        result
    };
    let (result, counts) = tokio::join!(work, long_turn(sender, upstream));
    let _ = client.close().await;
    (result, settled.into_inner(), counts)
}

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_duplex_long_reply_keeps_microphone_and_barge_in() {
    isolated("duplex::t3_a1_duplex_long_reply_keeps_microphone_and_barge_in", async {
        let setup = Fixture::new();
        let mut child = setup.device().stderr(Stdio::piped()).spawn().unwrap();
        let logs = BufReader::new(child.stderr.take().unwrap()).lines();
        let mut satellite = fixture::Satellite { child, logs };
        satellite.wait_for_log("wake=").await;
        let (result, settled, [quiet, first, second]) =
            timeout(OBSERVATION_TIMEOUT, Box::pin(call(&satellite))).await.unwrap();
        satellite.signal("-TERM").await;
        let exit = timeout(LIMIT, satellite.child.wait()).await.unwrap().unwrap();
        let logs = satellite.rest().await;
        let cuts: Vec<_> = settled.iter().map(|reply| reply.quality.cuts).collect();
        writeln!(
            io::stdout(),
            "DUPLEX upstream quiet={quiet} reply_first={first} reply_second={second} cuts={cuts:?} result={:?} exit={exit:?} device={logs:?}",
            result.as_ref().map(|stats| stats.reply_dropped)
        )
        .unwrap();
        // 20 ms frames: 2 s of delegation (after the wake word's replay) and
        // two 3 s halves of the reply.
        assert!((90..=180).contains(&quiet), "delegation frames {quiet}");
        assert!((140..=180).contains(&first), "first half frames {first}");
        assert!((140..=180).contains(&second), "second half frames {second}");
        assert_eq!(cuts, [1], "one barge cut");
        assert!(result.is_ok(), "the run ends: {result:?}");
        // The TCP listener keeps the fork's default TERM.
        assert_eq!(std::os::unix::process::ExitStatusExt::signal(&exit), Some(15));
        assert!(!logs.iter().any(|line| line.contains("mic: the client is not reading")), "{logs:?}");
    })
    .await;
}
