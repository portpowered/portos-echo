use super::*;
use echo_core_api::{BargeTrigger, SessionOutcome, SessionTurnEvent};
use tokio::time::Duration;

fn frame(level: i16) -> Vec<u8> {
    level.to_le_bytes().repeat(480)
}

/// A bounded media sender that takes `room` more frames, then refuses.
#[derive(Default)]
struct Narrow {
    room: std::cell::Cell<usize>,
    failed: std::cell::Cell<bool>,
    /// What the microphone hears of the reply frame last written.
    echo: std::cell::Cell<i16>,
    sent: std::cell::RefCell<Vec<i16>>,
}

impl VoicePort for Narrow {
    fn send(&self, _frame: PcmFrame) -> Result<(), Error> {
        // The evicting sender is never an acceptable path for speech.
        Err(Error::Audio)
    }
    fn try_send(&self, frame: PcmFrame) -> Result<bool, Error> {
        if self.failed.get() {
            return Err(Error::Unavailable);
        }
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

fn forward_test(source: &mut Source, marker: i16, port: &Narrow) -> Result<(), Error> {
    forward(&source.controller, &mut source.stats, &[marker; 320], port)
}

#[test]
fn setup_burst_waits_for_the_sender_in_order_and_mute_withholds_the_rest() {
    let mut source = Source::new(200, false).unwrap();
    let port = Narrow::default();
    port.room.set(2);
    for marker in 1..=5 {
        forward_test(&mut source, marker, &port).unwrap();
    }
    assert_eq!(*port.sent.borrow(), [1, 2]);
    assert!(source.uplink_pending());
    assert_eq!(source.stats.microphone_frames, 5);
    port.room.set(2);
    source.flush_uplink(&port).unwrap();
    assert_eq!(*port.sent.borrow(), [1, 2, 3, 4]);
    // Room for one: waiting speech goes first, the new frame queues behind it.
    port.room.set(1);
    forward_test(&mut source, 6, &port).unwrap();
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
    for marker in 0..=1500 {
        forward_test(&mut source, marker, &port).unwrap();
    }
    port.room.set(usize::MAX);
    source.flush_uplink(&port).unwrap();
    let sent = port.sent.borrow();
    assert_eq!(sent.as_slice(), (1..=1500).collect::<Vec<_>>());
    assert_eq!(source.stats.uplink_dropped, 1);
}

#[test]
fn sender_failure_leaves_the_oldest_unsent_frame_and_propagates_the_error() {
    let mut source = Source::new(200, false).unwrap();
    let port = Narrow::default();
    forward_test(&mut source, 11, &port).unwrap();
    port.failed.set(true);
    assert_eq!(forward_test(&mut source, 22, &port), Err(Error::Unavailable));
    assert!(source.uplink_pending());
    assert_eq!(source.stats.microphone_frames, 1);
    port.failed.set(false);
    port.room.set(2);
    source.flush_uplink(&port).unwrap();
    assert_eq!(*port.sent.borrow(), [11]);
    assert!(!source.uplink_pending());
}

#[tokio::test(start_paused = true)]
async fn acknowledgement_playout_returns_directly_to_screen_thinking() {
    use super::super::{ServeNotice, SessionNotice, voice_listen::playout_notice, voice_view::View};
    use crate::{
        bridge_health::Health, bridge_mcp::bridge_presentation::Presentation, bridge_runtime::bridge_screen::Screen,
        bridge_trace::Trace,
    };
    use echo_screen_contract::UiState;

    for queued_audio in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut trace = Trace::open(&directory.path().join("trace"), "chatgpt").unwrap();
        let health = Health::offline();
        let (send, mut states) = tokio::sync::mpsc::channel(8);
        let mut screen = Screen::bind(None).await.unwrap();
        screen.presentation = Presentation::new(Some(send));
        let mut view = View::new(&screen, &health, &mut trace);
        let mut source = Source::new(200, false).unwrap();
        source
            .controller
            .turn(|session| SessionTurnEvent::DispatchStarted { session });
        source.spoke();
        let mut mouth = ReplyAudio::mouth_fixture();
        mouth.test_handoff(&frame(1000));
        assert_eq!(
            source.controller.reply_ended().outcome,
            Some(Ok(SessionOutcome::Applied))
        );
        // The acknowledgement's quiet lasts the gap hold: the mouth closes.
        let frame_period = Duration::from_millis(20);
        let gap_hold = Duration::from_millis(300);
        let quiet = 15;
        if queued_audio {
            for _ in 0..quiet {
                mouth.test_handoff(&frame(0));
            }
            mouth.test_handoff(&frame(1000));
        }

        let mut project_mouth = |mouth: &ReplyAudio| {
            let notice = playout_notice(mouth, source.controller.dispatch_in_flight()).unwrap();
            view.observe(ServeNotice::Session(SessionNotice::Run(notice))).unwrap();
        };
        tokio::time::advance(Duration::from_millis(80)).await;
        project_mouth(&mouth);
        // Audio already queued past the hold closes the mouth with the last
        // sound; with none queued the close waits the hold for more.
        tokio::time::advance(if queued_audio {
            frame_period
        } else {
            frame_period + gap_hold
        })
        .await;
        project_mouth(&mouth);
        assert_eq!(mouth.voiced(), queued_audio);
        assert_eq!(states.try_recv().unwrap(), UiState::Speaking);
        assert_eq!(states.try_recv().unwrap(), UiState::Thinking);
        assert!(states.try_recv().is_err());

        if queued_audio {
            tokio::time::advance(frame_period * quiet).await;
            project_mouth(&mouth);
            assert_eq!(states.try_recv().unwrap(), UiState::Speaking);
            assert!(states.try_recv().is_err());
        }
    }
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
        super::super::media(MediaEvent::control(event), &mut port, &mut source, &mut |_| Ok(()))
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

/// The session heard "Stop."; the Echo played 160 ms more; the model yielded.
fn stopped() -> crate::bridge_runtime::bridge_pacing::Cut {
    crate::bridge_runtime::bridge_pacing::Cut {
        text: "Stop.".into(),
        silent_ms: 160,
        yield_ms: 900,
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
    stats.cut = Some(Box::new(stopped()));
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
    let cut = |count: u32, trigger: Option<&str>, at: Option<(u32, u32, &str)>| serde_json::json!({"count":count, "trigger":trigger, "holds":0, "hold_ms":0, "silent_ms":at.map(|at| at.0), "cut_ms":at.map(|at| at.1), "text":at.map(|at| at.2)});
    let stopped = cut(2, Some("user_transcript"), Some((160, 900, "Stop.")));
    assert_eq!(cuts, [cut(0, None, None), stopped]);
    assert_eq!(
        rows[6],
        serde_json::json!({"event":"timing", "elapsed_ms":0, "provider_auth_mode":"chatgpt", "run_id":"run-1",
            "name":"first_audio_after_endpoint_ms", "duration_ms":730, "endpoint":"user_transcript"})
    );
}

fn heard(level: i16) -> MicFrame {
    let format = echo_link::PcmFormat {
        rate: 16000,
        width: 2,
        channels: 1,
    };
    let payload = level.to_le_bytes().repeat(320);
    MicFrame {
        format,
        payload: payload.into(),
    }
}

/// One 20 ms step of a reply at `level`: the model's frame arrives, the one
/// that is due is written to the Echo, and the microphone hears a tenth of
/// what the Echo was last sent plus `voice`.
async fn replying(source: &mut Source, port: &Narrow, level: i16, voice: i16) {
    source.audio(&[level; 320]).unwrap();
    if source.playable()
        && let Ok(Some(frame)) = tokio::time::timeout(Duration::ZERO, source.reply.stage(true)).await
    {
        source.wrote();
        port.echo.set(i16::from_le_bytes([frame[0], frame[1]]) / 10);
    }
    source.capture(Some(heard(port.echo.get() + voice)), port).unwrap();
    tokio::time::advance(Duration::from_millis(20)).await;
}

/// T1, the owner's decision: the Live session alone judges a barge-in. On
/// the Echo the microphone hears the reply louder than the user, so 7 of 8
/// cuts came with nobody speaking. Speech at the microphone holds nothing and
/// every frame of it still goes up; the session's transcript holds the reply,
/// the model's yield cuts it, and the cut says what the session heard.
#[tokio::test(start_paused = true)]
async fn barge_in_is_judged_by_the_session_alone_and_its_cut_says_what_was_heard() {
    let mut source = Source::new(200, false).unwrap();
    let mut port = Narrow::default();
    port.room.set(usize::MAX);
    source.heard_user(BargeTrigger::UserTranscript, "What time is it?", true);
    source.spoke();
    for _ in 0..50 {
        replying(&mut source, &port, 2000, 0).await;
    }
    // The user speaks. The session has given no sign of it: nothing is held.
    for _ in 0..35 {
        replying(&mut source, &port, 2000, 6000).await;
        assert!(!source.controller.barge_pending() && source.playable());
    }
    // The session's transcript of that speech: the reply is held at once.
    source.heard_user(BargeTrigger::UserTranscript, "Sto", false);
    assert!(source.controller.barge_pending() && !source.playable());
    source.heard_user(BargeTrigger::UserTranscript, "p.", false);
    for _ in 0..26 {
        replying(&mut source, &port, 0, 6000).await;
    }
    source.interrupt(&mut port).unwrap();
    assert!(source.stale());
    let quality = source.settle(0);
    assert_eq!(
        (quality.cuts, quality.cut_trigger, quality.holds),
        (1, Some(BargeTrigger::UserTranscript), 1)
    );
    let cut = source.stats.cut.take().unwrap();
    assert_eq!((cut.text.as_str(), quality.mic.onsets), ("Stop.", 1));
    // The Echo played out what it had, at most its 200 ms lead, and the cut
    // came with the model's yield.
    assert!(cut.silent_ms <= 200 && (500..=540).contains(&cut.yield_ms), "{cut:?}");
    // Nothing was kept from the session: all 111 microphone frames went up.
    assert_eq!((source.stats.microphone_frames, source.uplink_pending()), (111, false));
    // The next reply is cut by nothing and carries no transcript.
    assert!(source.settle(0).cuts == 0 && source.stats.cut.is_none());
}

/// 200 ms of noise under 1.6 s of reply: the steps the reply was held for.
async fn noise(source: &mut Source, port: &Narrow) -> u32 {
    let mut held = 0;
    for step in 0..80 {
        replying(source, port, 2000, if step < 10 { 600 } else { 0 }).await;
        held += u32::from(source.controller.barge_pending());
    }
    held
}

/// T1: no sound at the microphone holds the reply or cuts it: not its own
/// echo, not a noise, not a voice as the model ends its turn.
#[tokio::test(start_paused = true)]
async fn barge_in_sound_at_the_microphone_never_holds_or_cuts_the_reply() {
    let mut source = Source::new(200, false).unwrap();
    let mut port = Narrow::default();
    port.room.set(usize::MAX);
    source.spoke();
    for level in [2000, 6000, 2000] {
        for _ in 0..50 {
            replying(&mut source, &port, level, 0).await;
            assert!(!source.controller.barge_pending(), "{level}");
        }
    }
    assert_eq!(noise(&mut source, &port).await, 0);
    for _ in 0..10 {
        replying(&mut source, &port, 2000, 6000).await;
    }
    source.reply.finish().unwrap();
    source.interrupt(&mut port).unwrap();
    assert!(source.barge_due().is_none() && !source.stale());
    let quality = source.settle(0);
    assert_eq!((quality.cuts, quality.holds, quality.discarded_audible), (0, 0, 0));
    assert_eq!((source.stats.reply_dropped, source.stats.cut.is_none()), (0, true));
}

/// T1, the device's lost start: a short answer was held from its first
/// frame by the transcript of the question, and dropped whole when it ended.
/// And its pause in every reply: the rest of that transcript came after the
/// answer's first audio and held it for the whole 2 s window.
#[tokio::test(start_paused = true)]
async fn reply_start_is_not_held_by_the_transcript_of_the_turn_it_answers() {
    let mut source = Source::new(200, false).unwrap();
    let port = Narrow::default();
    port.room.set(usize::MAX);
    source.spoke();
    // The question's transcript comes late: the answer has not sounded yet.
    source.heard_user(BargeTrigger::UserTranscript, "What", false);
    assert!(!source.controller.barge_pending());
    for _ in 0..30 {
        replying(&mut source, &port, 2000, 0).await;
    }
    // The rest of it comes later still, under the answer.
    source.heard_user(BargeTrigger::UserTranscript, " time is it?", false);
    source.heard_user(BargeTrigger::UserTranscript, "What time is it?", true);
    assert!(!source.controller.barge_pending() && source.playable());
    // A transcript now is the user talking over the answer.
    source.heard_user(BargeTrigger::UserTranscript, "", false);
    assert!(source.controller.barge_pending());
}

/// 600 ms of a reply at speaking level, with nobody else in the room.
async fn answering(source: &mut Source, port: &Narrow) {
    source.spoke();
    for _ in 0..30 {
        replying(source, port, 2000, 0).await;
    }
}

/// T1, the owner's report from the Echo: every reply spoke a little, paused
/// and then went on. `reply.quality` showed one hold of 2007 to 2041 ms in
/// each, and no endpoint before the first audio of a call's first reply: the
/// session's transcript of the question trailed the answer. Each reply owns
/// the first user turn transcribed since the reply before it.
#[tokio::test(start_paused = true)]
async fn reply_is_not_held_by_a_question_transcribed_after_its_first_audio() {
    let mut source = Source::new(200, false).unwrap();
    let mut port = Narrow::default();
    port.room.set(usize::MAX);
    // The first reply of a call: all of the question's transcript trails it.
    answering(&mut source, &port).await;
    source.heard_user(BargeTrigger::UserTranscript, "What", false);
    source.heard_user(BargeTrigger::UserTranscript, "What time is it?", true);
    assert!(!source.controller.barge_pending() && source.playable());
    // The next turn is the user talking over it. The model yields before
    // that turn's transcript is whole, and the reply is cut.
    source.heard_user(BargeTrigger::UserTranscript, "Sto", false);
    assert!(source.controller.barge_pending() && !source.playable());
    for _ in 0..26 {
        replying(&mut source, &port, 0, 0).await;
    }
    source.interrupt(&mut port).unwrap();
    let quality = source.settle(0);
    assert_eq!((quality.cuts, quality.holds), (1, 1));
    // The turn that cut it is the next reply's question: the rest of its
    // transcript holds nothing, and the turn after it does.
    source.controller.cut_settled();
    source.resume_reply(false).unwrap();
    answering(&mut source, &port).await;
    source.heard_user(BargeTrigger::UserTranscript, "Stop.", true);
    assert!(!source.controller.barge_pending() && source.playable());
    source.heard_user(BargeTrigger::UserTranscript, "No", false);
    assert!(source.controller.barge_pending());
    // The model talks through the window: the reply plays on and ends.
    for _ in 0..101 {
        replying(&mut source, &port, 2000, 0).await;
    }
    assert!(!source.controller.barge_pending());
    let quality = source.settle(0);
    assert_eq!((quality.cuts, quality.holds, quality.hold_ms), (0, 1, 2000));
    // A reply that ended awaits its own question again, as late as it comes.
    source.resume_reply(false).unwrap();
    answering(&mut source, &port).await;
    source.heard_user(BargeTrigger::UserTranscript, "And tomorrow?", true);
    assert!(!source.controller.barge_pending());
    let quality = source.settle(0);
    assert_eq!((quality.cuts, quality.holds, quality.hold_ms), (0, 0, 0));
}
