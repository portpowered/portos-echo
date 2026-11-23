//! T1: idle capture/control boundaries with staged memory-only input.
use super::*;
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
    let source = Source::new(200, false).unwrap();
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
    source
        .controller
        .configure_tools(Some(Duration::from_secs(3)), Duration::from_secs(12))
        .unwrap();
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
    assert!(!source.controller.ending());
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
            source
                .controller
                .turn(|session| echo_core_api::SessionTurnEvent::DispatchStarted { session });
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
            assert!(!source.controller.dispatch_in_flight());
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
