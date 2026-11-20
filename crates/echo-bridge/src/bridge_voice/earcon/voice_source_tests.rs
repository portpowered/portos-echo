use super::*;

fn frame(level: i16) -> Vec<u8> {
    level.to_le_bytes().repeat(480)
}

/// A bounded media sender that takes `room` more frames, then refuses.
#[derive(Default)]
struct Narrow {
    room: std::cell::Cell<usize>,
    sent: std::cell::RefCell<Vec<i16>>,
}

impl VoicePort for Narrow {
    fn send(&self, _frame: PcmFrame) -> Result<(), Error> {
        // The evicting sender is never an acceptable path for speech.
        Err(Error::Audio)
    }
    fn try_send(&self, frame: PcmFrame) -> Result<bool, Error> {
        if self.room.get() == 0 {
            return Ok(false);
        }
        self.room.set(self.room.get() - 1);
        self.sent.borrow_mut().push(frame[0]);
        Ok(true)
    }
    async fn next(&mut self) -> Result<crate::bridge_codex::MediaEvent, Error> {
        std::future::pending().await
    }
    fn ready_audio(&mut self) -> impl Future<Output = Result<Option<PcmFrame>, Error>> {
        std::future::ready(Ok(None))
    }
}

#[test]
fn setup_burst_waits_for_the_sender_in_order_and_mute_withholds_the_rest() {
    let mut source = Source::new(200, false).unwrap();
    let port = Narrow::default();
    port.room.set(2);
    for marker in 1..=5 {
        source.forward(&[marker; 320], &port).unwrap();
    }
    assert_eq!(*port.sent.borrow(), [1, 2]);
    assert!(source.uplink_pending());
    assert_eq!(source.stats.microphone_frames, 5);
    port.room.set(2);
    source.flush_uplink(&port).unwrap();
    assert_eq!(*port.sent.borrow(), [1, 2, 3, 4]);
    // Room for one: waiting speech goes first, the new frame queues behind it.
    port.room.set(1);
    source.forward(&[6; 320], &port).unwrap();
    assert_eq!(*port.sent.borrow(), [1, 2, 3, 4, 5]);
    assert!(source.uplink_pending());
    source.mute_uplink();
    port.room.set(5);
    source.flush_uplink(&port).unwrap();
    assert_eq!(*port.sent.borrow(), [1, 2, 3, 4, 5]);
    assert!(!source.uplink_pending());
    assert_eq!(source.stats.microphone_frames, 6);
}

#[test]
fn stalled_sender_backlog_is_bounded_and_keeps_the_newest_speech() {
    let mut source = Source::new(200, false).unwrap();
    let port = Narrow::default();
    for marker in 0..=UPLINK_BACKLOG {
        source.forward(&[i16::try_from(marker).unwrap(); 320], &port).unwrap();
    }
    port.room.set(usize::MAX);
    source.flush_uplink(&port).unwrap();
    let sent = port.sent.borrow();
    assert_eq!(sent.len(), UPLINK_BACKLOG);
    assert_eq!((sent[0], sent[UPLINK_BACKLOG - 1]), (1, 1500));
    assert_eq!(source.stats.uplink_dropped, 1);
}

#[tokio::test(start_paused = true)]
async fn mouth_opens_and_closes_on_the_echo_playout_clock_not_on_writes() {
    let mut mouth = Mouth::default();
    let t0 = Instant::now();
    // Silence first, then a lead burst of 5 audible frames written at t0.
    mouth.wrote(&frame(0), t0);
    for _ in 0..5 {
        mouth.wrote(&frame(QUIET.cast_signed() + 1), t0);
    }
    assert_eq!(mouth.due(), Some(t0 + FRAME + OUTPUT_DELAY));
    assert_eq!(mouth.poll(t0 + OUTPUT_DELAY), None);
    assert_eq!(mouth.poll(t0 + FRAME + OUTPUT_DELAY), Some(true));
    // Five audible frames after the leading silence finish at 120 ms.
    assert_eq!(mouth.due(), Some(t0 + FRAME * 6 + OUTPUT_DELAY));
    // Quiet frames keep the clock running but never reopen or extend it.
    mouth.wrote(&frame(-QUIET.cast_signed()), t0 + FRAME * 3);
    assert_eq!(mouth.poll(t0 + FRAME * 6 + OUTPUT_DELAY), Some(false));
    assert_eq!(mouth.due(), None);
}

#[tokio::test(start_paused = true)]
async fn mouth_restarts_the_clock_after_the_echo_ran_dry() {
    let mut mouth = Mouth::default();
    let t0 = Instant::now();
    mouth.wrote(&frame(1000), t0);
    assert_eq!(mouth.poll(t0 + OUTPUT_DELAY), Some(true));
    assert_eq!(mouth.poll(t0 + FRAME + OUTPUT_DELAY), Some(false));
    let later = t0 + Duration::from_secs(3);
    mouth.wrote(&frame(-1000), later);
    assert_eq!(mouth.due(), Some(later + OUTPUT_DELAY));
    assert_eq!(mouth.poll(later + OUTPUT_DELAY), Some(true));
}

#[tokio::test(start_paused = true)]
async fn mouth_quiet_thresholds_never_schedule_a_transition() {
    let mut mouth = Mouth::default();
    let t0 = Instant::now();
    for level in [0, 64, -64] {
        mouth.wrote(&frame(level), t0);
    }
    mouth.wrote(&[], t0);
    assert_eq!(mouth.due(), None);
    assert_eq!(mouth.poll(t0 + Duration::from_secs(1)), None);
}

#[tokio::test(start_paused = true)]
async fn mouth_preserves_quiet_gaps_even_when_all_audio_is_written_ahead() {
    let mut mouth = Mouth::default();
    let t0 = Instant::now();
    for level in [65, 0, -65] {
        mouth.wrote(&frame(level), t0);
    }
    for (offset, open) in [(0, true), (1, false), (2, true), (3, false)] {
        let due = t0 + OUTPUT_DELAY + FRAME * offset;
        assert_eq!(mouth.poll(due - Duration::from_nanos(1)), None);
        assert_eq!(mouth.poll(due), Some(open));
    }
    assert_eq!(mouth.due(), None);
}

#[tokio::test(start_paused = true)]
async fn first_audio_is_timed_from_the_user_transcript_or_the_delegated_turn() {
    use crate::bridge_codex::{Event, MediaEvent};
    use crate::bridge_runtime::bridge_pacing::{Endpoint, FirstAudio};
    let user = Event::Transcript {
        role: "user".into(),
        text: "fixture".into(),
        done: true,
    };
    let turn = Event::TurnStarted {
        thread: "thread-1".into(),
        turn: "turn-1".into(),
    };
    for (event, endpoint) in [(user, Endpoint::UserTranscript), (turn, Endpoint::DelegatedTurn)] {
        let mut source = Source::new(200, false).unwrap();
        let mut port = Narrow::default();
        super::super::media(MediaEvent::Control(event), &mut port, &mut source, &mut |_| Ok(()))
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(730)).await;
        source.meter.wrote(Instant::now());
        let sample = FirstAudio {
            after_endpoint_ms: 730,
            endpoint,
        };
        assert_eq!(source.meter.settle(0).first_audio, Some(sample));
    }
}

/// The view writes one `timing` record per settled reply that has a sample.
#[tokio::test(start_paused = true)]
async fn settled_reply_with_a_first_audio_sample_records_one_timing_row() {
    use crate::bridge_runtime::bridge_pacing::{Endpoint, FirstAudio};
    use crate::bridge_voice::{ServeNotice, SessionNotice, voice_view::View};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let mut trace = crate::bridge_trace::Trace::open(&path, "chatgpt").unwrap();
    let health = crate::bridge_health::Health::offline();
    let screen = crate::bridge_runtime::bridge_screen::Screen::bind(None).await.unwrap();
    let mut view = View::new(&screen, &health, &mut trace);
    view.observe(ServeNotice::Listening).unwrap();
    let mut stats = Stats::default();
    view.observe(ServeNotice::Session(SessionNotice::ReplyCompleted(stats.clone())))
        .unwrap();
    stats.quality.first_audio = Some(FirstAudio {
        after_endpoint_ms: 730,
        endpoint: Endpoint::UserTranscript,
    });
    stats.quality.target_min_ms = 100;
    stats.quality.target_max_ms = 300;
    stats.quality.target_final_ms = 280;
    stats.quality.reprimes = 1;
    stats.quality.concealment_ms = 20;
    stats.quality.fade_ins = 2;
    stats.quality.drained_frames = 3;
    (stats.quality.cuts, stats.quality.cut_trigger) = (2, Some(crate::bridge_voice::BargeTrigger::UserTranscript));
    view.observe(ServeNotice::Session(SessionNotice::ReplyCompleted(stats)))
        .unwrap();
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let events: Vec<_> = rows.iter().skip(2).map(|row| row["event"].as_str().unwrap()).collect();
    let reply = ["reply.played", "reply.quality"];
    assert_eq!(events, [reply.as_slice(), reply.as_slice(), &["timing"]].concat());
    // The adaptive playout's facts ride on `reply.quality`.
    let playout = |row: &serde_json::Value| {
        let fields = ["target_ms", "reprimes", "concealment_ms", "fade_ins", "drained_frames"];
        fields.map(|field| row[field].clone())
    };
    let target = |min: u32, max: u32, last: u32| serde_json::json!({"min":min, "max":max, "final":last});
    assert_eq!(
        playout(&rows[3]),
        [target(0, 0, 0), 0.into(), 0.into(), 0.into(), 0.into()]
    );
    assert_eq!(
        playout(&rows[5]),
        [target(100, 300, 280), 1.into(), 20.into(), 2.into(), 3.into()]
    );
    let cuts = [&rows[3], &rows[5]].map(|row| row["cuts"].clone());
    let cut = |count: u32, trigger: Option<&str>| serde_json::json!({"count":count, "trigger":trigger, "holds":0});
    assert_eq!(cuts, [cut(0, None), cut(2, Some("user_transcript"))]);
    assert_eq!(
        rows[6],
        serde_json::json!({"event":"timing", "elapsed_ms":0, "provider_auth_mode":"chatgpt", "run_id":"run-1",
            "name":"first_audio_after_endpoint_ms", "duration_ms":730, "endpoint":"user_transcript"})
    );
}
