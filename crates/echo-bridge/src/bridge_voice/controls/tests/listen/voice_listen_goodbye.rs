//! T1: an applied `end_session` during a delegated hand-off, with memory-only
//! device and media and paused time.
use super::super::super::session_tools::Grants;
use super::*;
use crate::bridge_codex::SessionCommand;

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

/// The farewell's tail can reach the bridge after the delegated turn the end
/// dropped has completed. That completion owes nothing and must not finish the
/// farewell early: its tail plays out in full, as an unbroken farewell does.
#[tokio::test(start_paused = true)]
async fn voice_listen_runner_end_plays_the_farewell_tail_after_the_dropped_turn_completes() {
    use Step::{Completed, End, Handoff, Played, Reply, Speak, Tail};
    for first in [false, true] {
        let whole = goodbye_with_end(&[Handoff(first), Reply, End, Played], 0).await;
        let split = goodbye_with_end(&[Handoff(first), Speak, End, Completed, Tail, Played], 0).await;
        assert!(whole > 0);
        assert_eq!(split, whole);
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
    /// The first half of a reply's audio, without its done.
    Speak,
    /// The rest of that reply's audio and its done.
    Tail,
    /// The running delegated turn completes without an answer.
    Completed,
}

/// Returns the PCM bytes the device played.
async fn goodbye_with_end(steps: &[Step], cuts: usize) -> usize {
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
    let cut = commands.iter().filter(|cmd| matches!(cmd, Command::Cut(_)));
    assert_eq!(cut.count(), cuts);
    played_bytes(&commands)
}

fn played_bytes(commands: &[Command]) -> usize {
    commands
        .iter()
        .map(|cmd| match cmd {
            Command::Frame(bytes) => bytes.len(),
            _ => 0,
        })
        .sum()
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
            Step::Speak => {
                let (role, text) = ("assistant".into(), "fixture".into());
                send_control(
                    tx,
                    Event::Transcript {
                        role,
                        text,
                        done: false,
                    },
                )
                .await;
                farewell_audio(tx).await;
            }
            Step::Tail => {
                farewell_audio(tx).await;
                send_control(tx, Event::ReplyDone { text: "fixture".into() }).await;
            }
            Step::Completed => {
                let (thread, turn) = ("thread".into(), "turn".into());
                send_control(
                    tx,
                    Event::TurnCompleted {
                        thread,
                        turn,
                        answered: false,
                    },
                )
                .await;
            }
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

/// Half of the audio `reply` sends.
async fn farewell_audio(tx: &mpsc::Sender<MediaEvent>) {
    for _ in 0..6 {
        tx.send(MediaEvent::Audio(Box::new([5000; 320]))).await.unwrap();
    }
}

async fn send_control(tx: &mpsc::Sender<MediaEvent>, event: Event) {
    tx.send(MediaEvent::Control(event)).await.unwrap();
}
