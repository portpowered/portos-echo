//! T1: idle capture/control boundaries with staged memory-only input.
use super::super::super::session_tools::Grants;
use super::*;
use crate::bridge_codex::SessionCommand;
use echo_link::{MicFrame, MicrophoneRun, RunStats};

struct Run(mpsc::Receiver<MicFrame>);
impl MicrophoneRun for Run {
    fn wake_phrase(&self) -> &'static str {
        "Computer"
    }
    async fn recv(&mut self) -> Result<Option<MicFrame>, echo_link::Error> {
        Ok(self.0.recv().await)
    }
    fn stats(&self) -> RunStats {
        RunStats::default()
    }
}

struct Events(mpsc::Receiver<LinkEvent<Run>>);
impl LinkEvents for Events {
    type Run = Run;
    async fn recv(&mut self) -> Result<Option<LinkEvent<Run>>, echo_link::Error> {
        Ok(self.0.recv().await)
    }
}

struct Input {
    microphone: Run,
    events: Events,
    capture: mpsc::Sender<MicFrame>,
    controls: mpsc::Sender<LinkEvent<Run>>,
    turn_started: bool,
}
impl Input {
    fn new() -> Self {
        let (capture, microphone) = mpsc::channel(4);
        let (controls, events) = mpsc::channel(4);
        Self {
            microphone: Run(microphone),
            events: Events(events),
            capture,
            controls,
            turn_started: false,
        }
    }

    async fn step(&mut self, port: &mut Port, source: &mut Source) -> Result<bool, Error> {
        super::super::listening_step(
            (&mut self.events, &mut self.microphone),
            port,
            source,
            &mut self.turn_started,
            &mut |_| Ok(()),
        )
        .await
    }

    async fn samples(&self, value: i16, count: usize) {
        self.capture
            .send(MicFrame {
                format: crate::bridge_audio::format(16_000),
                payload: value.to_le_bytes().repeat(count),
            })
            .await
            .unwrap();
    }
}

fn source() -> Source {
    let mut source = Source::new(200, false).unwrap();
    source
        .controller
        .admit_response(Instant::now() + Duration::from_secs(10))
        .unwrap();
    source.controller.reply_started();
    source.controller.played(true).unwrap();
    source.controller.listen_after_reply(Duration::from_secs(3)).unwrap();
    source
}

#[tokio::test(start_paused = true)]
async fn voice_listen_mute_discards_mic_during_farewell_without_touching_playback() {
    let mut input = Input::new();
    let (_tx, rx) = mpsc::channel(1);
    let mut port = Port(rx, std::rc::Rc::default());
    let mut source = Source::new(200, false).unwrap();
    source.session = Grants::new(Some(Duration::from_secs(3)), Duration::from_secs(12));
    source.spoke();
    source.audio(&[5000; 320]).unwrap();
    let muted = Cell::new(false);
    let mut observe = |notice| {
        if matches!(notice, Notice::Device(LinkEvent::Mute(true))) {
            muted.set(true);
        }
        Ok(())
    };
    input.controls.send(LinkEvent::Mute(true)).await.unwrap();
    let event = input.events.recv().await.unwrap().unwrap();
    super::super::super::device(&event, false, &mut source, &mut observe).unwrap();
    input.samples(1234, 320).await;
    let frame = input.microphone.recv().await.unwrap();
    assert!(source.capture(frame, &port).unwrap());
    assert!(muted.get());
    assert!(port.1.borrow().is_empty());
    super::super::super::media(
        MediaEvent::Control(Event::ReplyDone {
            text: "Goodbye.".into(),
        }),
        &mut port,
        &mut source,
        &mut observe,
    )
    .await
    .unwrap();
    assert!(source.reply.finished());
    assert!(!source.controller.cutting());
    assert_eq!(source.stats.reply_received, 1);
    assert!(!source.session.ending());
}

#[tokio::test(start_paused = true)]
async fn settings_restart_waits_for_delegation_and_beats_ready_followup() {
    let restart = crate::bridge_settings::settings_restart::Restart::default();
    restart
        .scope(async {
            let mut input = Input::new();
            let (tx, rx) = mpsc::channel(2);
            let mut port = Port(rx, std::rc::Rc::default());
            let mut source = source();
            source.delegation.started();
            restart.request();
            {
                let mut drain = None;
                let mut observe = |_| Ok(());
                let work = super::super::listen(
                    (&mut input.events, &mut input.microphone),
                    &mut port,
                    (&mut source, &mut drain),
                    true,
                    std::future::pending(),
                    &mut observe,
                );
                tokio::pin!(work);
                poll_fn(|cx| {
                    assert!(work.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
                tx.send(MediaEvent::Control(Event::TurnCompleted {
                    thread: "fixture".into(),
                    turn: "turn".into(),
                    answered: true,
                }))
                .await
                .unwrap();
                tx.send(MediaEvent::Control(Event::SpeechStarted)).await.unwrap();
                assert_eq!(work.await, Ok(false));
            }
            assert!(!source.delegation.in_flight());
            assert!(source.controller.has_played());
            assert!(matches!(
                port.0.try_recv(),
                Ok(MediaEvent::Control(Event::SpeechStarted))
            ));
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn voice_listen_capture_continues_and_mute_discards_partial_pcm_without_renewal() {
    let mut input = Input::new();
    let (_tx, rx) = mpsc::channel(1);
    let mut port = Port(rx, std::rc::Rc::default());
    let mut source = source();
    let deadline = source.controller.idle_deadline();
    input.samples(1234, 640).await;
    assert_eq!(input.step(&mut port, &mut source).await, Ok(false));
    assert_eq!(*port.1.borrow(), vec![[1234; 320]; 2]);
    input.samples(4321, 160).await;
    assert_eq!(input.step(&mut port, &mut source).await, Ok(false));
    input.controls.send(LinkEvent::Mute(true)).await.unwrap();
    assert_eq!(input.step(&mut port, &mut source).await, Ok(false));
    assert!(source.muted);
    assert!(source.microphone.is_none());
    input.samples(9999, 640).await;
    assert_eq!(input.step(&mut port, &mut source).await, Ok(false));
    assert_eq!(*port.1.borrow(), vec![[1234; 320]; 2]);
    input.controls.send(LinkEvent::Mute(false)).await.unwrap();
    assert_eq!(input.step(&mut port, &mut source).await, Ok(false));
    input.samples(-1234, 320).await;
    assert_eq!(input.step(&mut port, &mut source).await, Ok(false));
    assert_eq!(*port.1.borrow(), vec![[1234; 320], [1234; 320], [-1234; 320]]);
    assert_eq!(source.stats.microphone_frames, 3);
    assert_eq!(source.stats.microphone_chunks, 4);
    assert_eq!(source.controller.idle_deadline(), deadline);
}

#[tokio::test(start_paused = true)]
async fn voice_listen_activity_cancels_idle_and_admits_a_fresh_response_window() {
    for event in [
        Event::SpeechStarted,
        Event::Transcript {
            role: "user".into(),
            text: "second turn".into(),
            done: true,
        },
        Event::Transcript {
            role: "assistant".into(),
            text: "second reply".into(),
            done: false,
        },
    ] {
        let mut input = Input::new();
        let (tx, rx) = mpsc::channel(1);
        let mut port = Port(rx, std::rc::Rc::default());
        let mut source = source();
        let start = Instant::now();
        tokio::time::advance(Duration::from_millis(2999)).await;
        tx.send(MediaEvent::Control(event)).await.unwrap();
        assert_eq!(input.step(&mut port, &mut source).await, Ok(true));
        assert_eq!(source.controller.idle_deadline(), None);
        assert!(!source.controller.has_played());
        assert_eq!(
            source.controller.replacement_deadline(),
            Ok(start + Duration::from_millis(12_999))
        );
        assert!(source.controller.accepts_audio());
    }
}

#[tokio::test(start_paused = true)]
async fn voice_listen_due_expiry_wins_over_ready_activity_without_consuming_it() {
    let mut input = Input::new();
    let (tx, rx) = mpsc::channel(1);
    let mut port = Port(rx, std::rc::Rc::default());
    let mut source = source();
    let deadline = source.controller.idle_deadline().unwrap();
    tx.send(MediaEvent::Control(Event::SpeechStarted)).await.unwrap();
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(
        super::super::listen(
            (&mut input.events, &mut input.microphone),
            &mut port,
            (&mut source, &mut None),
            true,
            std::future::pending(),
            &mut |_| Ok(()),
        )
        .await,
        Ok(false)
    );
    assert_eq!(source.controller.idle_deadline(), Some(deadline));
    assert!(source.controller.has_played());
    assert!(matches!(
        port.0.try_recv(),
        Ok(MediaEvent::Control(Event::SpeechStarted))
    ));
}

#[tokio::test(start_paused = true)]
async fn voice_listen_transport_eof_and_closed_call_never_become_idle_success() {
    for device_eof in [false, true] {
        let input = Input::new();
        let (tx, rx) = mpsc::channel(1);
        let mut port = Port(rx, std::rc::Rc::default());
        let mut source = source();
        let deadline = source.controller.idle_deadline();
        let mut events = input.events;
        let mut microphone = input.microphone;
        if device_eof {
            drop(input.controls);
        } else {
            tx.send(MediaEvent::Control(Event::Closed)).await.unwrap();
        }
        let result = super::super::listen(
            (&mut events, &mut microphone),
            &mut port,
            (&mut source, &mut None),
            true,
            std::future::pending(),
            &mut |_| Ok(()),
        )
        .await;
        assert_eq!(
            result,
            Err(if device_eof { Error::Wyoming } else { Error::Unavailable })
        );
        assert_eq!(source.controller.idle_deadline(), deadline);
        assert_eq!(port.1.borrow().len(), 0);
    }
}

/// An applied `end_session` supersedes everything (owner, 2026-11-21): the
/// delegated turn still running owes nothing more, and the run closes as soon
/// as the farewell already being spoken has played, uncut, instead of waiting
/// for the turn's answer.
#[tokio::test(start_paused = true)]
async fn voice_listen_runner_end_keeps_the_farewell_and_drops_the_running_delegation() {
    use Step::{End, Handoff, Played, Reply};
    for first in [false, true] {
        goodbye_with_end(&[Handoff(first), Reply, End, Played], 0).await;
    }
}

/// An end before any farewell audio still cuts the opened playback at once.
#[tokio::test(start_paused = true)]
async fn voice_listen_runner_end_before_the_farewell_closes_at_once() {
    use Step::{End, Handoff};
    for first in [false, true] {
        goodbye_with_end(&[Handoff(first), End], 1).await;
    }
}

/// After a played reply, an end during the next hand-off closes at once, with
/// or without speech in the gap and whether or not the voice answered first.
#[tokio::test(start_paused = true)]
async fn voice_listen_runner_end_after_played_closes_without_another_listen_window() {
    use Step::{Asked, End, Handoff, Heard, Played, Reply};
    for first in [false, true] {
        goodbye_with_end(&[Reply, Played, Heard, Handoff(first), End], 1).await;
        goodbye_with_end(&[Reply, Played, Heard, Asked, Handoff(first), End], 1).await;
        goodbye_with_end(&[Reply, Played, Heard, Asked, Handoff(first), Reply, End, Played], 0).await;
    }
}

/// One scripted input. `Handoff(true)` sends the turn start before the marker.
#[derive(Clone, Copy)]
enum Step {
    Handoff(bool),
    End,
    Reply,
    Played,
    Heard,
    Asked,
}

async fn goodbye_with_end(steps: &[Step], cuts: usize) {
    let device = Observer::default();
    let mut client = FakeConnector::new(device.clone()).connect().await.unwrap();
    let mut events = client.events().unwrap();
    device.wake("Computer", vec![]);
    let Some(LinkEvent::Wake(mut microphone)) = events.recv().await.unwrap() else {
        unreachable!()
    };
    let (tx, rx) = mpsc::channel(64);
    let mut port = Port(rx, std::rc::Rc::default());
    let mut source = Source::new(200, false).unwrap();
    let options = Options {
        listen_timeout: Some(Duration::from_secs(3)),
        session_max: Duration::from_secs(60),
        response_deadline: Instant::now() + Duration::from_secs(10),
        played_timeout: Duration::from_secs(1),
        reply_lead_ms: 200,
        muted: false,
    };
    source.session = Grants::new(options.listen_timeout, options.session_max);
    source.controller.admit_response(options.response_deadline).unwrap();
    let (end, answer) = session_request(SessionCommand::End, &source.session.run, "farewell");
    let played = tokio::sync::Notify::new();
    let stopped = tokio::sync::Notify::new();
    let mut receipts = Vec::new();
    let mut observe = |notice| {
        match notice {
            Notice::SessionTool(receipt) => receipts.push(receipt.outcome),
            Notice::ReplyPlayed => played.notify_one(),
            _ => (),
        }
        Ok(())
    };
    let started = Instant::now();
    let answer = goodbye_run(
        replies(
            &client,
            (&mut events, &mut microphone),
            &mut port,
            (&mut source, &mut None),
            &options,
            std::future::pending(),
            &mut observe,
        ),
        &device,
        &stopped,
        goodbye_inputs(&device, &tx, (&stopped, &played), (end, answer), steps),
    )
    .await;
    assert_eq!(answer.content, serde_json::json!({"ending_after_reply": true}));
    assert_eq!(receipts, ["applied"]);
    // The running turn held the run to its 60 s ceiling; the end did not wait
    // for it, nor for the response deadline or an idle window.
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert!(!source.delegation.in_flight());
    assert!(source.controller.ready_to_close(true, true));
    let commands = device.commands();
    assert_eq!(commands.iter().filter(|cmd| matches!(cmd, Command::Cut(_))).count(), cuts);
}

async fn goodbye_run<T>(
    run: impl Future<Output = Result<(), Error>>,
    device: &Observer,
    stopped: &tokio::sync::Notify,
    inputs: impl Future<Output = T>,
) -> T {
    tokio::pin!(run);
    let (mut stops, mut cuts) = (0, 0);
    let run = poll_fn(|cx| {
        let result = run.as_mut().poll(cx);
        let commands = device.commands();
        let seen = commands.iter().filter(|cmd| matches!(cmd, Command::Stop)).count();
        if seen > stops {
            stops = seen;
            stopped.notify_one();
        }
        // The device acknowledges a cut as it acknowledges a finished reply.
        let cut = commands.iter().filter(|cmd| matches!(cmd, Command::Cut(_))).count();
        if cut > cuts {
            cuts = cut;
            device.finish(Ok(()));
            cx.waker().wake_by_ref();
        }
        result.map(Result::unwrap)
    });
    let ((), answer) = tokio::join!(run, inputs);
    answer
}

async fn goodbye_inputs(
    device: &Observer,
    tx: &mpsc::Sender<MediaEvent>,
    (stopped, played): (&tokio::sync::Notify, &tokio::sync::Notify),
    (end, answer): (Event, tokio::sync::oneshot::Receiver<echo_agent_api::ToolResult>),
    steps: &[Step],
) -> echo_agent_api::ToolResult {
    let mut end = Some(end);
    for step in steps {
        match step {
            Step::Handoff(turn_first) => handoff(tx, *turn_first).await,
            Step::End => send_control(tx, end.take().unwrap()).await,
            Step::Reply => reply(tx, 5000).await,
            Step::Played => {
                stopped.notified().await;
                device.finish(Ok(()));
            }
            Step::Heard => played.notified().await,
            Step::Asked => {
                let (role, text) = ("user".into(), "what day is it?".into());
                send_control(tx, Event::Transcript { role, text, done: true }).await;
            }
        }
    }
    answer.await.unwrap()
}

async fn handoff(tx: &mpsc::Sender<MediaEvent>, turn_first: bool) {
    let turn = Event::TurnStarted {
        thread: "thread".into(),
        turn: "turn".into(),
    };
    let marker = Event::ReplyHandoff(echo_contracts::ids::DelegationId::new("voice-handoff").unwrap());
    let events = if turn_first { [turn, marker] } else { [marker, turn] };
    for event in events {
        send_control(tx, event).await;
    }
}

async fn send_control(tx: &mpsc::Sender<MediaEvent>, event: Event) {
    tx.send(MediaEvent::Control(event)).await.unwrap();
}
