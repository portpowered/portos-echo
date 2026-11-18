//! Test-only wall-clock observation; never drives the bridge's fake clock.
use std::{future::Future, time::Duration};
use std::{io::Write, time::Instant};
use tokio::{sync::watch, time::Instant as Deadline};

// Initial unloaded cold maxima: Windows bridge 3.208 s, connected increment
// 0.594 s; Linux bridge 1.320 s, connected increment 0.545 s. Ten seconds
// per startup phase leaves >3x bridge and >16x session headroom for contention.
// These are ceilings, not synchronization delays; nextest still enforces 30 s.
pub(super) const STARTUP: Duration = Duration::from_secs(10);
// Observed media increment <=0.151 s and receipt increment <=0.972 s on Windows;
// Linux <=0.150 s and <=0.912 s. Keep steady state tight (3 s / 4 s),
// independently of the cold-start allowance. Loaded proof remains necessary.
pub(super) const MEDIA: Duration = Duration::from_secs(3);
pub(super) const CLEANUP: Duration = Duration::from_secs(4);

#[derive(Clone, Copy)]
struct State {
    name: &'static str,
    last: &'static str,
    deadline: Deadline,
}

#[derive(Clone)]
pub(super) struct Phase(watch::Sender<State>);

impl Phase {
    pub(super) fn new(name: &'static str, budget: Duration) -> Self {
        Self(
            watch::channel(State {
                name,
                last: "none observed",
                deadline: Deadline::now() + budget,
            })
            .0,
        )
    }

    // Only an observed readiness boundary starts a new phase. Intermediate
    // events update diagnostics without extending the absolute deadline.
    pub(super) fn next(&self, name: &'static str, budget: Duration) {
        self.0.send_replace(State {
            name,
            last: "none observed",
            deadline: Deadline::now() + budget,
        });
    }

    pub(super) fn seen(&self, event: &'static str) {
        self.0.send_modify(|state| state.last = event);
    }

    pub(super) async fn wait<T>(&self, work: impl Future<Output = Result<T, String>>) -> Result<T, String> {
        let mut changes = self.0.subscribe();
        tokio::pin!(work);
        loop {
            let state = *changes.borrow_and_update();
            if Deadline::now() >= state.deadline {
                return Err(Self::error(state, "deadline expired"));
            }
            tokio::select! {
                biased;
                _ = changes.changed() => {},
                () = tokio::time::sleep_until(state.deadline) => {
                    return Err(Self::error(*changes.borrow(), "deadline expired"));
                }
                result = &mut work => {
                    return result.map_err(|cause| Self::error(*changes.borrow(), &cause));
                }
            }
        }
    }

    fn error(state: State, cause: &str) -> String {
        format!("m4 phase '{}': {cause}; last event: {}", state.name, state.last)
    }
}

pub(super) fn finish<T>(primary: Result<T, String>, cleanup: Result<(), String>) -> Result<T, String> {
    match (primary, cleanup) {
        (Err(primary), Err(cleanup)) => Err(format!("{primary}; cleanup: {cleanup}")),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

#[derive(Clone, Copy)]
pub(super) struct Measurement {
    start: Instant,
    negative: bool,
    emotion: bool,
}

impl Measurement {
    pub(super) fn new(negative: bool, emotion: bool) -> Self {
        Self {
            start: Instant::now(),
            negative,
            emotion,
        }
    }

    pub(super) fn mark(self, phase: &'static str) {
        // Emit each completed boundary immediately so a later aggregate timeout
        // cannot erase the partial/censored observation. Only fixed labels and
        // synthetic case flags are logged, never process output or credentials.
        writeln!(
            std::io::stdout(),
            "M4 phase: {}",
            serde_json::json!({
                "phase": phase, "elapsed_ms": self.start.elapsed().as_secs_f64() * 1000.0,
                "negative": self.negative, "emotion": self.emotion,
            })
        )
        .unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn m4_phase_matching_signal_completes_once() {
    let phase = Phase::new("bridge ready", STARTUP);
    let calls = std::cell::Cell::new(0);
    let result = phase
        .wait(async {
            calls.set(calls.get() + 1);
            phase.seen("bridge.ready");
            Ok(42)
        })
        .await;
    assert_eq!(result, Ok(42));
    assert_eq!(calls.get(), 1);
}

#[tokio::test(start_paused = true)]
async fn m4_phase_absent_signal_names_phase_and_none() {
    let phase = Phase::new("provider/session ready", STARTUP);
    let start = Deadline::now();
    let result = phase.wait(std::future::pending::<Result<(), String>>()).await;
    assert_eq!(
        result,
        Err("m4 phase 'provider/session ready': deadline expired; last event: none observed".into())
    );
    assert_eq!(start.elapsed(), STARTUP);
}

#[tokio::test(start_paused = true)]
async fn m4_phase_partial_events_never_extend_deadline_or_advance_next_stage() {
    let phase = Phase::new("media done", MEDIA);
    let start = Deadline::now();
    let next_stage = std::cell::Cell::new(0);
    let result = phase
        .wait(async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            phase.seen("satellite.audio-chunk");
            std::future::pending::<()>().await;
            next_stage.set(next_stage.get() + 1);
            Ok(())
        })
        .await;
    assert_eq!(
        result,
        Err("m4 phase 'media done': deadline expired; last event: satellite.audio-chunk".into())
    );
    assert_eq!(start.elapsed(), MEDIA);
    assert_eq!(next_stage.get(), 0);
}

#[tokio::test(start_paused = true)]
async fn m4_phase_readiness_starts_only_the_next_phase_budget() {
    let phase = Phase::new("provider/session ready", STARTUP);
    let start = Deadline::now();
    let result = phase
        .wait(async {
            tokio::time::sleep(Duration::from_secs(9)).await;
            phase.seen("screen.connected");
            phase.next("media done", MEDIA);
            phase.seen("screen.speaking");
            std::future::pending::<Result<(), String>>().await
        })
        .await;
    assert_eq!(
        result,
        Err("m4 phase 'media done': deadline expired; last event: screen.speaking".into())
    );
    assert_eq!(start.elapsed(), Duration::from_secs(12));
}

#[tokio::test(start_paused = true)]
async fn m4_phase_expired_deadline_does_not_poll_ready_work() {
    let phase = Phase::new("tools listed", Duration::ZERO);
    let polled = std::cell::Cell::new(false);
    let result = phase
        .wait(async {
            polled.set(true);
            Ok(())
        })
        .await;
    assert_eq!(
        result,
        Err("m4 phase 'tools listed': deadline expired; last event: none observed".into())
    );
    assert!(!polled.get());
}

#[tokio::test(start_paused = true)]
async fn m4_phase_eof_retains_cause_and_primary_survives_cleanup_failure() {
    let phase = Phase::new("media done", MEDIA);
    phase.seen("screen.speaking");
    let primary = phase
        .wait(async { Err::<(), _>("screen: EOF before observation".into()) })
        .await;
    let result = finish(primary, Err("owned cleanup failed".into()));
    assert_eq!(result, Err("m4 phase 'media done': screen: EOF before observation; last event: screen.speaking; cleanup: owned cleanup failed".into()));
}

#[tokio::test(start_paused = true)]
async fn m4_phase_cancelling_wait_keeps_caller_ownership_and_next_stage_pending() {
    let phase = Phase::new("media done", MEDIA);
    let owner = std::rc::Rc::new(());
    let next_stage = std::cell::Cell::new(false);
    let borrowed = &owner;
    {
        let waiting = phase.wait(async {
            std::future::pending::<()>().await;
            next_stage.set(true);
            Ok(std::rc::Rc::strong_count(borrowed))
        });
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), &mut waiting)
                .await
                .is_err()
        );
    }
    assert_eq!(std::rc::Rc::strong_count(&owner), 1);
    assert!(!next_stage.get());
    assert_eq!(finish(Ok(7), Ok(())), Ok(7));
    assert_eq!(finish(Ok(7), Err("cleanup".into())), Err("cleanup".into()));
}

#[tokio::test(start_paused = true)]
async fn m4_phase_ready_before_deadline_and_bounded_cleanup() {
    let phase = Phase::new("media done", MEDIA);
    let ready = phase
        .wait(async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(9600)
        })
        .await;
    assert_eq!(ready, Ok(9600));
    phase.next("receipts collected", CLEANUP);
    let start = Deadline::now();
    let cleanup = phase.wait(std::future::pending::<Result<(), String>>()).await;
    assert_eq!(start.elapsed(), CLEANUP);
    assert_eq!(finish(Err::<(), _>("primary media failure".into()), cleanup),
        Err("primary media failure; cleanup: m4 phase 'receipts collected': deadline expired; last event: none observed".into()));
}
