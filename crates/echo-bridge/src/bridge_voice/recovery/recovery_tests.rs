//! T1: the retry policy and the kept link on the paused clock, memory only.
use super::*;
use echo_link::{
    EchoLink, LinkEvents, MicrophoneRun,
    fake::{Command, FakeConnector, Observer},
};
use std::collections::VecDeque;

fn failed(error: Error) -> Round {
    Round {
        result: Err(error),
        ready: None,
        served: false,
        killed: false,
    }
}

/// The kill switch: the one failure after readiness that ends the owner.
fn killed() -> Round {
    Round {
        killed: true,
        ..failed(Error::Control)
    }
}

/// Scripted generations; each one may use the kept link like the service does.
struct Script<F> {
    rounds: VecDeque<Round>,
    each: F,
    starts: Vec<Instant>,
    reports: Vec<(String, u32, Duration, bool)>,
    devices: Vec<&'static str>,
    requested: std::rc::Rc<std::cell::Cell<bool>>,
}

impl<F> Script<F> {
    fn new(rounds: Vec<Round>, each: F) -> Self {
        Self {
            rounds: rounds.into(),
            each,
            starts: vec![],
            reports: vec![],
            devices: vec![],
            requested: std::rc::Rc::default(),
        }
    }

    fn delays(&self) -> Vec<u64> {
        self.starts
            .windows(2)
            .map(|pair| u64::try_from((pair[1] - pair[0]).as_millis()).unwrap())
            .collect()
    }
}

impl<F: AsyncFnMut(usize)> Stage<FakeConnector> for Script<F> {
    async fn generation(&mut self, _: &Retained<FakeConnector>, _: Pin<&mut impl Future<Output = ()>>) -> Round {
        self.starts.push(Instant::now());
        (self.each)(self.starts.len()).await;
        self.rounds.pop_front().unwrap()
    }

    fn requested(&self) -> bool {
        self.requested.get()
    }

    async fn recovering(&mut self, error: &Error, attempt: u32, delay: Duration, linked: bool) {
        self.reports.push((error.to_string(), attempt, delay, linked));
    }

    fn device(&mut self, event: LinkEvent<()>) {
        self.devices.push(match event {
            LinkEvent::Mute(true) => "muted",
            LinkEvent::Mute(false) => "unmuted",
            LinkEvent::MicrophoneDropped(_) => "dropped",
            _ => unreachable!(),
        });
    }
}

fn link() -> (Observer, Retained<FakeConnector>) {
    let observer = Observer::default();
    (observer.clone(), Retained::new(FakeConnector::new(observer)))
}

#[test]
fn backoff_doubles_from_one_second_to_the_cap_and_never_overflows() {
    let mut backoff = Backoff::default();
    let delays: Vec<_> = (0..10).map(|_| backoff.next().1.as_secs()).collect();
    assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60, 60, 60]);
    for _ in 0..100 {
        assert_eq!(backoff.next().1, DELAY_CAP);
    }
    backoff.reset();
    assert_eq!(backoff.next(), (1, Duration::from_secs(1)));
}

// Live (any post-startup dependency) failing N times in a row waits exactly
// the schedule between attempts, and one success restarts it at one second.
#[tokio::test(start_paused = true)]
async fn consecutive_failures_wait_the_exact_schedule_and_success_resets_it() {
    let (_observer, link) = link();
    let mut rounds: Vec<_> = (0..9).map(|_| failed(Error::Unavailable)).collect();
    // The ninth generation answered a wake before it failed.
    rounds[8].ready = Some(Instant::now());
    rounds[8].served = true;
    rounds.extend([failed(Error::Deadline), killed()]);
    let mut stage = Script::new(rounds, async |_| ());
    let result = supervise(&mut stage, &link, std::future::pending()).await;
    assert_eq!(result, Err(Error::Control));
    assert_eq!(
        stage.delays(),
        [1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000, 1_000, 2_000]
    );
    let attempts: Vec<_> = stage.reports.iter().map(|report| report.1).collect();
    assert_eq!(attempts, [1, 2, 3, 4, 5, 6, 7, 8, 1, 2]);
    assert_eq!(stage.reports[9], ("deadline".into(), 2, Duration::from_secs(2), false));
}

#[tokio::test(start_paused = true)]
async fn a_generation_that_stayed_ready_for_the_cap_restarts_the_schedule() {
    let (_observer, link) = link();
    let start = Instant::now();
    let mut stage = Script::new(
        vec![
            failed(Error::Unavailable),
            failed(Error::Unavailable),
            Round {
                result: Err(Error::Wyoming),
                ready: Some(start),
                served: false,
                killed: false,
            },
            killed(),
        ],
        async |generation| {
            if generation == 3 {
                tokio::time::sleep(DELAY_CAP).await;
            }
        },
    );
    assert_eq!(
        supervise(&mut stage, &link, std::future::pending()).await,
        Err(Error::Control)
    );
    // The third start includes the minute it ran; its retry waits one second.
    assert_eq!(stage.delays(), [1_000, 2_000, 61_000]);
}

#[tokio::test(start_paused = true)]
async fn only_a_stop_and_startup_configuration_are_terminal() {
    use echo_mcp_auth::AuthError;
    for error in [
        Error::Input,
        Error::CodexHomeRequired,
        Error::CodexHome,
        Error::CodexBinary,
        Error::CodexCaFile,
        Error::CodexImport,
        Error::Authorization,
        Error::Mcp(AuthError::RelinkRequired),
    ] {
        assert!(terminal(&error, false, false), "{error}");
        assert!(!terminal(&error, true, false), "{error}");
        assert!(terminal(&error, true, true), "{error}");
    }
    for error in [
        Error::Audio,
        Error::Wyoming,
        Error::Unavailable,
        Error::Deadline,
        Error::NoResponse,
        Error::Decode,
        Error::Trace,
        Error::Health,
        Error::Screen,
        Error::Mcp(AuthError::Transport),
    ] {
        assert!(!terminal(&error, false, false), "{error}");
        assert!(!terminal(&error, true, false), "{error}");
    }
    // A muted or cut run reports `control` without any stop: it is retried.
    assert!(!terminal(&Error::Control, true, false));
    assert!(terminal(&Error::Control, true, true));
    // A credential revoked after readiness is retried at the cap, never fatal.
    let (_observer, link) = link();
    let mut rounds = vec![Round {
        result: Err(Error::Unavailable),
        ready: Some(Instant::now()),
        served: false,
        killed: false,
    }];
    rounds.extend((0..8).map(|_| failed(Error::Authorization)));
    rounds.push(killed());
    let mut stage = Script::new(rounds, async |_| ());
    assert_eq!(
        supervise(&mut stage, &link, std::future::pending()).await,
        Err(Error::Control)
    );
    assert_eq!(stage.delays()[6..], [60_000, 60_000, 60_000]);
    assert_eq!(stage.reports[8].0, "authorization");
}

// Stdin EOF during a retry delay is the same clean stop as at any other time.
#[tokio::test(start_paused = true)]
async fn a_requested_stop_during_a_retry_delay_is_a_clean_stop() {
    let (observer, link) = link();
    let mut stage = Script::new(vec![failed(Error::Unavailable)], async |_| {
        link.connect().await.unwrap();
    });
    let requested = stage.requested.clone();
    let start = Instant::now();
    let result = supervise(&mut stage, &link, async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        requested.set(true);
    })
    .await;
    assert_eq!(result, Ok(()));
    assert_eq!(start.elapsed(), Duration::from_millis(400));
    assert_eq!(stage.starts.len(), 1);
    assert_eq!(observer.commands(), [Command::Connect, Command::Probe, Command::Close]);
}

// The device connection outlives failed generations: one connect, one capture
// registration, and a wake that arrives during the delay is answered at once.
#[tokio::test(start_paused = true)]
async fn a_wake_during_the_delay_starts_the_next_generation_on_the_kept_link() {
    let (observer, link) = link();
    let wakes = std::cell::RefCell::new(vec![]);
    let device = observer.clone();
    let mut stage = Script::new(
        vec![failed(Error::Unavailable), failed(Error::Unavailable), killed()],
        async |generation| {
            let mut client = link.connect().await.unwrap();
            client.enable_capture(30).await.unwrap();
            client.close().await.unwrap();
            match generation {
                // Fails idle; the wake below lands 300 ms into the 1 s delay.
                1 => {
                    let device = device.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        device.mute(true);
                        device.mute(false);
                        device.wake("held", vec![]);
                    });
                }
                // Admits the held wake, then fails with its run still open.
                2 => {
                    let mut events = client.events().unwrap();
                    let Some(LinkEvent::Wake(run)) = events.recv().await.unwrap() else {
                        unreachable!()
                    };
                    wakes.borrow_mut().push(run.wake_phrase().to_owned());
                }
                _ => assert_eq!(link.muted(), Some(false)),
            }
        },
    );
    assert_eq!(
        supervise(&mut stage, &link, std::future::pending()).await,
        Err(Error::Control)
    );
    assert_eq!(*wakes.borrow(), ["held"]);
    assert_eq!(stage.delays(), [300, 2_000]);
    assert_eq!(stage.devices, ["muted", "unmuted"]);
    assert!(stage.reports.iter().all(|report| report.3));
    assert_eq!(
        observer.commands(),
        [
            Command::Connect,
            Command::Capture,
            Command::Probe,
            Command::FailRun(RunFailure::Unavailable),
            Command::Probe,
            Command::Close
        ]
    );
}

struct Refusing(Observer, std::sync::atomic::AtomicBool);

impl LinkConnector for Refusing {
    type Link = echo_link::fake::FakeLink;

    async fn connect(&self) -> Result<Self::Link, echo_link::Error> {
        if self.1.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(echo_link::Error::Transport);
        }
        FakeConnector::new(self.0.clone()).connect().await
    }
}

#[tokio::test(start_paused = true)]
async fn a_refused_connection_is_retried_and_a_held_wake_is_refused_on_the_device() {
    let observer = Observer::default();
    let link = Retained::new(Refusing(observer.clone(), true.into()));
    assert!(link.connect().await.is_err());
    assert_eq!(link.muted(), None);
    assert!(!link.settle(RunFailure::Unavailable, true).await);
    link.rest(Duration::from_secs(1), |_| unreachable!()).await;
    let mut client = link.connect().await.unwrap();
    assert!(client.events().is_ok());
    // The stream is back after its borrower dropped; a second borrow is busy.
    let events = client.events().ok();
    assert!(events.is_some() && client.events().err() == Some(echo_link::Error::Busy));
    drop(events);
    observer.wake("unanswered", vec![]);
    link.rest(Duration::from_secs(1), |_| unreachable!()).await;
    assert!(link.settle(RunFailure::RelinkRequired, true).await);
    assert!(!link.settle(RunFailure::Unavailable, false).await);
    assert_eq!(link.muted(), None);
    assert_eq!(
        observer.commands(),
        [
            Command::Connect,
            Command::FailRun(RunFailure::RelinkRequired),
            Command::Probe,
            Command::Probe,
            Command::Close
        ]
    );
}

// One device-side failure keeps the connection; a second in a row replaces it.
#[tokio::test(start_paused = true)]
async fn a_repeated_device_failure_dials_a_fresh_connection() {
    let (observer, link) = link();
    let mut stage = Script::new(
        vec![failed(Error::Wyoming), failed(Error::Wyoming), killed()],
        async |_| {
            link.connect().await.unwrap();
        },
    );
    assert_eq!(
        supervise(&mut stage, &link, std::future::pending()).await,
        Err(Error::Control)
    );
    let linked: Vec<_> = stage.reports.iter().map(|report| report.3).collect();
    assert_eq!(linked, [true, false]);
    assert_eq!(
        observer.commands(),
        [
            Command::Connect,
            Command::Probe,
            Command::Probe,
            Command::Close,
            Command::Connect,
            Command::Close
        ]
    );
}
