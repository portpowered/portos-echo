//! T1: the session hold tool on the real run loop, with the Echo's own
//! windows: a 15 s listen window and the default 600 s session bound. An
//! `extend_session` grant keeps a silent run listening to its deadline, also
//! after an answered question, and an explicit end during the grant still
//! closes the run at once. Without the grant the run ends at its listen window.
use super::super::super::Stats;
use super::*;
use crate::bridge_codex::SessionCommand;
use std::sync::Arc;

struct RunPort<'a> {
    media: Port,
    owner: &'a RefCell<Option<Arc<()>>>,
}

impl VoicePort for RunPort<'_> {
    fn send(&self, frame: echo_media::PcmFrame) -> Result<(), Error> {
        self.media.send(frame)
    }

    fn bind_session(&mut self, run: Option<Arc<()>>) {
        *self.owner.borrow_mut() = run;
    }

    async fn next(&mut self) -> Result<MediaEvent, Error> {
        self.media.next().await
    }

    fn ready_audio(&mut self) -> impl Future<Output = Result<Option<echo_media::PcmFrame>, Error>> {
        self.media.ready_audio()
    }
}

/// "Hold on for two minutes": the reply plays, then the agent's
/// `extend_session(120)` is applied while the run listens in silence, also
/// when an answered question came first.
#[tokio::test(start_paused = true)]
async fn voice_listen_hold_grant_keeps_a_silent_run_to_its_deadline() {
    for answered in [false, true] {
        let end = hold(answered, Then::WaitOut).await;
        assert_eq!(end.result, Ok(()));
        // Not cut at the 15 s listen window, alive at 119 s, and over exactly
        // at the granted deadline: 120 s beyond the listen window it extended.
        assert!(end.alive_at_119);
        assert!(end.deadline - end.granted >= Duration::from_secs(120));
        assert_eq!(end.ended, end.deadline);
        assert_eq!(end.receipts, ["extend_session:applied"]);
    }
}

/// The device run of 2026-11-21: "What time is it?" was answered, then "Hold
/// on for two minutes." got only a spoken acknowledgement, with no hand-off
/// and so no grant. A listening run cannot tell that from a goodbye: it ends
/// 15 s after the acknowledgement played. Only the `extend_session` grant
/// above holds it, so the Live guidance makes the hold hand-off mandatory.
#[tokio::test(start_paused = true)]
async fn voice_listen_hold_acknowledged_without_a_grant_ends_at_the_listen_window() {
    let end = hold(true, Then::Acknowledge).await;
    assert_eq!(end.result, Ok(()));
    assert!(!end.alive_at_119);
    assert_eq!(end.ended - end.granted, Duration::from_secs(15));
    assert_eq!(end.receipts, Vec::<String>::new());
}

/// An explicit end 60 s into the 120 s grant closes the run at once.
#[tokio::test(start_paused = true)]
async fn voice_listen_explicit_end_during_a_hold_closes_at_once() {
    for answered in [false, true] {
        let end = hold(answered, Then::End(Duration::from_secs(60))).await;
        assert_eq!(end.result, Ok(()));
        assert_eq!(end.ended - end.granted, Duration::from_secs(60));
        assert!(end.ended < end.deadline);
        assert_eq!(end.receipts, ["extend_session:applied", "end_session:applied"]);
    }
}

/// What follows the hold's acknowledgement.
#[derive(Clone, Copy)]
enum Then {
    /// The grant, then silence to its deadline.
    WaitOut,
    /// The grant, then an explicit end after this long.
    End(Duration),
    /// No grant: only the acknowledgement was spoken.
    Acknowledge,
}

struct Hold {
    result: Result<(), Error>,
    alive_at_119: bool,
    granted: Instant,
    deadline: Instant,
    ended: Instant,
    receipts: Vec<String>,
}

/// What the run reports: played replies and session tool receipts.
#[derive(Default)]
struct Seen {
    played: Cell<usize>,
    receipts: RefCell<Vec<String>>,
}

impl Seen {
    fn record(&self, notice: Notice) {
        match notice {
            Notice::ReplyPlayed => self.played.set(self.played.get() + 1),
            Notice::SessionTool(receipt) => {
                let entry = format!("{}:{}", receipt.tool, receipt.outcome);
                self.receipts.borrow_mut().push(entry);
            }
            _ => (),
        }
    }
}

fn options() -> Options {
    Options {
        listen_timeout: Some(Duration::from_secs(15)),
        session_max: Duration::from_secs(600),
        reply_lead_ms: 200,
        response_deadline: Instant::now() + Duration::from_secs(10),
        played_timeout: Duration::from_secs(1),
        muted: false,
    }
}

/// One run: optionally an answered question first, then the hold's
/// acknowledgement, then `then`. Without a grant, `granted` is when the
/// acknowledgement played and `deadline` its listen window.
async fn hold(answered: bool, then: Then) -> Hold {
    let observer = Observer::default();
    let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    observer.wake("Computer", vec![]);
    let Some(LinkEvent::Wake(microphone)) = events.recv().await.unwrap() else {
        unreachable!()
    };
    let (tx, rx) = mpsc::channel(64);
    let owner = RefCell::new(None);
    let mut port = RunPort {
        media: Port(rx, std::rc::Rc::default()),
        owner: &owner,
    };
    let opened = Instant::now();
    let seen = Seen::default();
    let mut work = Box::pin(super::super::super::run(
        &client,
        &mut events,
        microphone,
        &mut port,
        options(),
        std::future::pending(),
        |notice| {
            seen.record(notice);
            Ok(())
        },
    ));
    acknowledge(&tx, &mut work, &observer, &seen.played, answered).await;
    let run = owner.borrow().clone().unwrap();
    let (granted, deadline, result, alive_at_119) = follow(then, &tx, work, &observer, (&run, opened)).await;
    assert!(observer.commands().contains(&Command::EndRun));
    Hold {
        result,
        alive_at_119,
        granted,
        deadline,
        ended: Instant::now(),
        receipts: seen.receipts.take(),
    }
}

/// Optionally "What time is it?" answered and the user heard again, then the
/// hold's acknowledgement played.
async fn acknowledge<W: Future<Output = Result<Stats, Error>>>(
    tx: &mpsc::Sender<MediaEvent>,
    work: &mut Pin<Box<W>>,
    observer: &Observer,
    played: &Cell<usize>,
    answered: bool,
) {
    if answered {
        reply(tx, 5000).await;
        super::settle(work, observer, played, 1).await;
        tokio::time::advance(Duration::from_secs(2)).await;
        tx.send(MediaEvent::Control(Event::SpeechStarted)).await.unwrap();
        poll(work.as_mut()).await;
    }
    reply(tx, -5000).await;
    super::settle(work, observer, played, 1 + usize::from(answered)).await;
}

/// What follows the acknowledgement: (granted, deadline, result, alive at 119 s).
async fn follow<W: Future<Output = Result<Stats, Error>>>(
    then: Then,
    tx: &mpsc::Sender<MediaEvent>,
    mut work: Pin<Box<W>>,
    observer: &Observer,
    (run, opened): (&Arc<()>, Instant),
) -> (Instant, Instant, Result<(), Error>, bool) {
    if matches!(then, Then::Acknowledge) {
        let played = Instant::now();
        let deadline = played + Duration::from_secs(15);
        let (result, alive) = wait_out(work, observer, deadline).await;
        return (played, deadline, result, alive);
    }
    let (granted, deadline) = extend(tx, &mut work, run, opened).await;
    match then {
        Then::End(after) => (granted, deadline, end_explicitly(tx, work, run, after).await, false),
        Then::WaitOut | Then::Acknowledge => {
            let (result, alive) = wait_out(work, observer, deadline).await;
            (granted, deadline, result, alive)
        }
    }
}

/// The agent's `extend_session(120)`; returns when it was granted and the
/// deadline the grant reports.
async fn extend<W: Future<Output = Result<Stats, Error>>>(
    tx: &mpsc::Sender<MediaEvent>,
    work: &mut Pin<Box<W>>,
    run: &Arc<()>,
    opened: Instant,
) -> (Instant, Instant) {
    let (extend, granted) = session_request(SessionCommand::Extend(120), run, "hold");
    tx.send(MediaEvent::Control(extend)).await.unwrap();
    poll(work.as_mut()).await;
    let granted_at = Instant::now();
    let grant = granted.await.unwrap();
    assert_eq!(grant.content["capped"], false);
    let deadline_ms = grant.content["deadline_ms"].as_u64().unwrap();
    (granted_at, opened + Duration::from_millis(deadline_ms))
}

async fn end_explicitly<W: Future<Output = Result<Stats, Error>>>(
    tx: &mpsc::Sender<MediaEvent>,
    mut work: Pin<Box<W>>,
    run: &Arc<()>,
    after: Duration,
) -> Result<(), Error> {
    tokio::time::advance(after).await;
    poll(work.as_mut()).await;
    let (end, ended) = session_request(SessionCommand::End, run, "end");
    tx.send(MediaEvent::Control(end)).await.unwrap();
    let result = work.await.map(|_| ());
    let answer = ended.await.unwrap().content;
    assert_eq!(answer, serde_json::json!({"ending_after_reply": true}));
    result
}

/// Silence to `deadline`: whether the run is alive 119 s in (false when the
/// deadline comes sooner), then alive just before the deadline and over at it.
async fn wait_out<W: Future<Output = Result<Stats, Error>>>(
    mut work: Pin<Box<W>>,
    observer: &Observer,
    deadline: Instant,
) -> (Result<(), Error>, bool) {
    let probe = Duration::from_secs(119);
    let alive = Instant::now() + probe < deadline && {
        tokio::time::advance(probe).await;
        poll(work.as_mut()).await;
        !observer.commands().contains(&Command::EndRun)
    };
    let early = deadline.checked_duration_since(Instant::now()).unwrap();
    tokio::time::advance(early.checked_sub(Duration::from_millis(1)).unwrap()).await;
    poll(work.as_mut()).await;
    assert!(!observer.commands().contains(&Command::EndRun));
    tokio::time::advance(Duration::from_millis(1)).await;
    (work.await.map(|_| ()), alive)
}
