//! T1: within a run the face stays awake. A real mouth model on a paused clock,
//! the production playout notices and the production projection; the received
//! states are what the screen server streams over SSE. Reply audio drives
//! Speaking; between replies and in gaps the run shows Connected (mouth
//! closed), and only the run's end publishes Ready.
use super::*;
use crate::bridge_mcp::bridge_presentation::Presentation;
use crate::{bridge_codex::ReplyAudio, bridge_voice::voice_listen::playout_notice};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};

type Timeline = Vec<(u64, UiState)>;

fn frame(level: i16) -> Vec<u8> {
    level.to_le_bytes().repeat(480)
}

/// Queue `count` 20 ms frames on the Echo now; they play back to back.
fn write(mouth: &mut ReplyAudio, count: u64, level: i16) {
    for _ in 0..count {
        mouth.test_handoff(&frame(level));
    }
}

struct Run {
    start: Instant,
    mouth: ReplyAudio,
    receive: mpsc::Receiver<UiState>,
    timeline: Timeline,
}

impl Run {
    fn collect(&mut self) {
        let at = u64::try_from((Instant::now() - self.start).as_millis()).unwrap();
        while let Ok(state) = self.receive.try_recv() {
            self.timeline.push((at, state));
        }
    }

    /// Play out everything queued on the mouth, projecting each notice.
    async fn play(&mut self, view: &mut View<'_>) {
        while let Some(due) = self.mouth.mouth_due() {
            tokio::time::advance(due.saturating_duration_since(Instant::now())).await;
            if let Some(notice) = playout_notice(&self.mouth, false) {
                view.observe(ServeNotice::Session(SessionNotice::Run(notice))).unwrap();
                self.collect();
            }
        }
    }

    fn complete(&mut self, view: &mut View<'_>) {
        for notice in [Notice::ReplySettled(Stats::default()), Notice::ReplyPlayed] {
            view.observe(ServeNotice::Session(SessionNotice::Run(notice))).unwrap();
        }
        view.observe(ServeNotice::Session(SessionNotice::Completed(Stats::default())))
            .unwrap();
        self.collect();
    }
}

fn ready() -> UiState {
    UiState::Ready {
        message: "Echo Live ready".into(),
    }
}

/// One run: phrases of 500 ms audio separated by queued quiet `gaps`, then
/// `silence` of nothing at all before the run completes.
async fn run(gaps: &[u64], silence: u64) -> Timeline {
    let directory = tempfile::tempdir().unwrap();
    let mut trace = Trace::open(&directory.path().join("trace"), "chatgpt").unwrap();
    let health = Health::offline();
    let (send, receive) = mpsc::channel(64);
    let mut screen = Screen::bind(None).await.unwrap();
    screen.presentation = Presentation::new(Some(send));
    let mut view = View::new(&screen, &health, &mut trace);
    let mut harness = Run {
        start: Instant::now(),
        mouth: ReplyAudio::mouth_fixture(),
        receive,
        timeline: Vec::new(),
    };
    view.observe(ServeNotice::Listening).unwrap();
    harness.collect();
    write(&mut harness.mouth, 25, 1000);
    for gap in gaps {
        write(&mut harness.mouth, gap / 20, 0);
        write(&mut harness.mouth, 25, 1000);
    }
    // The whole reply is written: its last sound closes the mouth.
    harness.mouth.mouth_settle();
    harness.play(&mut view).await;
    tokio::time::advance(Duration::from_millis(silence)).await;
    harness.complete(&mut view);
    harness.timeline
}

#[tokio::test(start_paused = true)]
async fn a_600_ms_gap_closes_the_mouth_without_leaving_the_run() {
    // Audio 0-500 ms and 1100-1600 ms: the mouth opens 80 ms after each
    // phrase starts and closes 80 ms after its last frame has played.
    let expected = [
        (0, UiState::Listening),
        (80, UiState::Speaking),
        (580, UiState::Connected),
        (1180, UiState::Speaking),
        (1680, UiState::Connected),
        (1680, ready()),
    ];
    assert_eq!(run(&[600], 0).await, expected);
}

/// A breath between words is not the end of speech: gaps shorter than
/// 300 ms never leave Speaking.
#[tokio::test(start_paused = true)]
async fn gaps_shorter_than_300_ms_stay_speaking() {
    // Audio 0-500, 520-1020, 1220-1720 and 2000-2500 ms.
    let expected = [
        (0, UiState::Listening),
        (80, UiState::Speaking),
        (2580, UiState::Connected),
        (2580, ready()),
    ];
    assert_eq!(run(&[20, 200, 280], 0).await, expected);
}

#[tokio::test(start_paused = true)]
async fn several_gaps_alternate_speaking_and_connected_and_ready_waits_for_the_end() {
    let timeline = run(&[300, 600, 900], 0).await;
    let states: Vec<_> = timeline.iter().map(|(_, state)| state.clone()).collect();
    let mut expected = vec![UiState::Listening];
    for _ in 0..4 {
        expected.extend([UiState::Speaking, UiState::Connected]);
    }
    expected.push(ready());
    assert_eq!(states, expected);
    let times: Vec<_> = timeline.iter().map(|(at, _)| *at).collect();
    // Phrases start at 0, 800, 1900 and 3300 ms.
    assert_eq!(times, [0, 80, 580, 880, 1380, 1980, 2480, 3380, 3880, 3880]);
}

#[tokio::test(start_paused = true)]
async fn silence_after_the_reply_holds_connected_until_the_run_completes() {
    let expected = [
        (0, UiState::Listening),
        (80, UiState::Speaking),
        (580, UiState::Connected),
        (2080, ready()),
    ];
    assert_eq!(run(&[], 1500).await, expected);
}
