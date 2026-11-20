//! T1 playback and hand-off permutations on the same run owner.
use super::*;

#[tokio::test(start_paused = true)]
async fn voice_listen_goodbye_settles_without_arming_idle_in_either_tool_order() {
    for order in [GoodbyeOrder::Early, GoodbyeOrder::LateTool] {
        goodbye_with_end(order).await;
    }
}

#[tokio::test(start_paused = true)]
async fn voice_listen_goodbye_handoff_after_played_uses_the_settled_reply() {
    for provisional in [false, true] {
        for turn_first in [false, true] {
            goodbye_with_end(GoodbyeOrder::LateHandoff {
                turn_first,
                provisional,
            })
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn voice_listen_only_final_standalone_user_turns_on_admitted_builtin_voice_hold() {
    use crate::bridge_voice::prompts;
    let root = tempfile::tempdir().unwrap();
    let voice = root.path().join("prompts/voice.md");
    let mut trace = Some(crate::bridge_trace::Trace::open(&root.path().join("trace"), "api_key").unwrap());
    prompts::scope(Some(root.path()), async {
        for (contents, built_in) in [
            (None, true),
            (Some(" "), true),
            (Some(echo_config::LIVE_INSTRUCTIONS), false),
            (Some(" "), false),
        ] {
            if let Some(contents) = contents {
                std::fs::write(&voice, contents).unwrap();
            }
            let ((_, text), admitted) = prompts::snapshot("unused", "unused", &mut trace)?;
            assert_eq!(text, echo_config::LIVE_INSTRUCTIONS);
            assert_eq!(admitted, built_in);
            check_utterances(admitted).await;
        }
        Ok(())
    })
    .await
    .unwrap();
}

async fn check_utterances(built_in: bool) {
    let (_tx, rx) = mpsc::channel(1);
    let mut port = Port(rx, std::rc::Rc::default());
    for (role, text, done, matches) in [
        ("user", "goodbye", true, true),
        ("user", "  THANKS, THAT’S ALL!  ", true, true),
        ("user", "goodbye", false, false),
        ("user", "thanks", true, false),
        ("user", "thanks, and what time is it?", true, false),
        ("user", "thanks, that's all. What time is it?", true, false),
        ("assistant", "goodbye", true, false),
    ] {
        let mut source = Source::new(200, false).unwrap();
        source.built_in_voice = built_in;
        source.session = Grants::new(Some(Duration::from_secs(3)), Duration::from_secs(60));
        let event = Event::Transcript {
            role: role.into(),
            text: text.into(),
            done,
        };
        crate::bridge_voice::media(MediaEvent::Control(event), &mut port, &mut source, &mut |_| Ok(()))
            .await
            .unwrap();
        assert_eq!(waiting_for_goodbye(&source), built_in && matches, "{role} {text}");
        assert!(!source.session.ending());
        assert!(source.session.goodbye.is_none());
    }
}

#[derive(Clone, Copy)]
enum GoodbyeOrder {
    Early,
    LateTool,
    LateHandoff { turn_first: bool, provisional: bool },
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
    source.built_in_voice = matches!(order, GoodbyeOrder::LateHandoff { provisional: true, .. });
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
    assert_eq!(
        source.controller.idle_deadline().is_some(),
        matches!(order, GoodbyeOrder::LateHandoff { provisional: false, .. })
    );
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
    if matches!(order, GoodbyeOrder::LateHandoff { .. }) {
        send_control(
            tx,
            Event::Transcript {
                role: "user".into(),
                text: "thanks, that's all".into(),
                done: true,
            },
        )
        .await;
    } else {
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
    after_goodbye_played(tx, order, end).await;
    answer.await.unwrap()
}

async fn after_goodbye_played(tx: &mpsc::Sender<MediaEvent>, order: GoodbyeOrder, end: Option<Event>) {
    if let GoodbyeOrder::LateHandoff {
        turn_first,
        provisional,
    } = order
    {
        if provisional {
            // This entire burst arrives before the marker. The device keeps
            // its one playback and the receipt keeps its original run owner.
            send_control(tx, Event::SpeechStarted).await;
            reply(tx, -5000).await;
        }
        handoff(tx, turn_first).await;
    }
    if let Some(end) = end {
        send_control(tx, Event::SpeechStarted).await;
        send_control(tx, end).await;
    }
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
