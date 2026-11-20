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
        }
    }

    async fn step(&mut self, port: &mut Port, source: &mut Source) -> Result<bool, Error> {
        super::super::listening_step(&mut self.events, &mut self.microphone, port, source, &mut |_| Ok(())).await
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
async fn voice_listen_goodbye_timeout_retires_end_authority_without_admitting_speech() {
    let mut input = Input::new();
    let (tx, rx) = mpsc::channel(2);
    let mut port = Port(rx, std::rc::Rc::default());
    let mut source = Source::new(200, false).unwrap();
    source.session = Grants::new(Some(Duration::from_secs(3)), Duration::from_secs(12));
    source.controller.reply_started();
    source.controller.played(true).unwrap();
    goodbye_handoff(&mut source);
    let old = source.session.run.clone();
    let mut receipts = vec![];
    let mut observe = |notice| {
        if let Notice::SessionTool(receipt) = notice {
            receipts.push(receipt.outcome);
        }
        Ok(())
    };
    let (event, answer) = session_request(SessionCommand::Invalid { end: true }, &old, "refused");
    tx.send(MediaEvent::Control(event)).await.unwrap();
    tx.send(MediaEvent::Control(Event::SpeechStarted)).await.unwrap();
    wait_goodbye(
        (&mut input.events, &mut input.microphone),
        &mut port,
        (&mut source, &mut None),
        Instant::now() + Duration::from_secs(1),
        std::future::pending(),
        &mut observe,
    )
    .await
    .unwrap();
    assert_eq!(
        answer.await.unwrap().content,
        serde_json::json!({"error":"invalid_arguments"})
    );
    assert_eq!(source.controller.idle_deadline(), None);
    assert!(source.controller.has_played());
    let (event, answer) = session_request(SessionCommand::End, &old, "obsolete");
    tx.send(MediaEvent::Control(event)).await.unwrap();
    goodbye_step(
        &mut input.events,
        &mut input.microphone,
        &mut port,
        &mut source,
        &mut observe,
    )
    .await
    .unwrap();
    assert_eq!(
        answer.await.unwrap().content,
        serde_json::json!({"error":"session_stale"})
    );
    assert!(!source.session.ending());
    assert_eq!(receipts, ["invalid_arguments", "session_stale"]);
}

#[tokio::test(start_paused = true)]
async fn voice_listen_mute_discards_mic_during_farewell_without_touching_playback() {
    let mut input = Input::new();
    let (_tx, rx) = mpsc::channel(1);
    let mut port = Port(rx, std::rc::Rc::default());
    let mut source = Source::new(200, false).unwrap();
    source.session = Grants::new(Some(Duration::from_secs(3)), Duration::from_secs(12));
    source.spoke();
    goodbye_handoff(&mut source);
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

#[tokio::test(start_paused = true)]
async fn voice_listen_goodbye_settles_without_arming_idle_in_either_tool_order() {
    for order in [GoodbyeOrder::Early, GoodbyeOrder::LateTool] {
        goodbye_with_end(order).await;
    }
}

#[tokio::test(start_paused = true)]
async fn voice_listen_goodbye_handoff_after_played_uses_the_settled_reply() {
    for turn_first in [false, true] {
        goodbye_with_end(GoodbyeOrder::LateHandoff { turn_first }).await;
    }
}

#[derive(Clone, Copy)]
enum GoodbyeOrder {
    Early,
    LateTool,
    LateHandoff { turn_first: bool },
}

async fn goodbye_with_end(order: GoodbyeOrder) {
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
        goodbye_inputs(&device, &tx, (&stopped, &played), (end, answer), order),
    )
    .await;
    assert_eq!(answer.content, serde_json::json!({"ending_after_reply": true}));
    assert_eq!(receipts, ["applied"]);
    if !matches!(order, GoodbyeOrder::LateHandoff { .. }) {
        assert_eq!(source.controller.idle_deadline(), None);
    }
    assert!(source.controller.ready_to_close(true, true));
    assert_eq!(source.stats.reply_sent, 12);
    let commands = device.commands();
    let starts = commands.iter().filter(|cmd| matches!(cmd, Command::Start(..))).count();
    assert_eq!(starts, 1);
    assert!(!commands.iter().any(|cmd| matches!(cmd, Command::Cut(_))));
}

async fn goodbye_run<T>(
    run: impl Future<Output = Result<(), Error>>,
    device: &Observer,
    stopped: &tokio::sync::Notify,
    inputs: impl Future<Output = T>,
) -> T {
    tokio::pin!(run);
    let mut stop_seen = false;
    let run = poll_fn(|cx| {
        let result = run.as_mut().poll(cx);
        if !stop_seen && device.commands().last() == Some(&Command::Stop) {
            stop_seen = true;
            stopped.notify_one();
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
    order: GoodbyeOrder,
) -> echo_agent_api::ToolResult {
    if !matches!(order, GoodbyeOrder::LateHandoff { .. }) {
        handoff(tx, true).await;
    }
    let end = if matches!(order, GoodbyeOrder::Early) {
        send_control(tx, end).await;
        None
    } else {
        Some(end)
    };
    reply(tx, 5000).await;
    stopped.notified().await;
    device.finish(Ok(()));
    played.notified().await;
    if let GoodbyeOrder::LateHandoff { turn_first } = order {
        handoff(tx, turn_first).await;
    }
    if let Some(end) = end {
        send_control(tx, Event::SpeechStarted).await;
        send_control(tx, end).await;
    }
    answer.await.unwrap()
}

async fn handoff(tx: &mpsc::Sender<MediaEvent>, turn_first: bool) {
    let turn = Event::TurnStarted {
        thread: "thread".into(),
        turn: "turn".into(),
    };
    let marker = Event::GoodbyeHandoff;
    let events = if turn_first { [turn, marker] } else { [marker, turn] };
    for event in events {
        send_control(tx, event).await;
    }
}

async fn send_control(tx: &mpsc::Sender<MediaEvent>, event: Event) {
    tx.send(MediaEvent::Control(event)).await.unwrap();
}
