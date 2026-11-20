//! T1: a pending context append never releases the next wake on failure.
use super::*;
use std::io::Write;

const ZONE: &str = "America/Los_Angeles";
const INITIAL: &str = "Live prompt\n[Call-start context: 2026-02-03 Tuesday 03:30:05 UTC+00:00; operator time zone: America/Los_Angeles; answer time questions in America/Los_Angeles]"; // lint-rules: allow-literal synthetic UTC clock text
const BASE: &str = "Client audio bridge\n[Call-start context: 2026-02-03 Tuesday 03:30:05 UTC+00:00; operator time zone: America/Los_Angeles; answer time questions in America/Los_Angeles]"; // lint-rules: allow-literal synthetic UTC clock text
const LATER: &str = "[Wake-time context: 2026-02-03 Tuesday 03:37:05 UTC+00:00; operator time zone: America/Los_Angeles; answer time questions in America/Los_Angeles]"; // lint-rules: allow-literal synthetic UTC clock text

fn t0() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + Duration::from_secs(1_770_089_405)
}

fn assert_no_reply(stats: &[crate::bridge_voice::Stats], observer: &Observer) {
    assert_eq!(
        stats
            .iter()
            .map(|s| (s.reply_received, s.reply_sent))
            .collect::<Vec<_>>(),
        vec![(0, 0); 2]
    );
    assert_eq!(
        observer
            .commands()
            .iter()
            .filter(|command| matches!(command, Command::Frame(_)))
            .count(),
        0
    );
}

#[tokio::test(start_paused = true)]
async fn voice_retained_wake_time_refresh_reuses_call_and_discards_old_pcm() {
    use crate::bridge_voice::TEST_WAKE_TIME;
    TEST_WAKE_TIME.scope(std::cell::Cell::new(t0()), refreshed_wake()).await;
}

async fn refreshed_wake() {
    let (mut io, mut server) = fixture();
    let (peer, state, audio) = peer(true, None);
    let (link, mut events, microphone, observer) = device().await;
    let (again, mut receive) = mpsc::unbounded_channel::<()>();
    let completed = Arc::new(Mutex::new(Vec::new()));
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        start_call(&mut call).await;
        conversation(
            (&link, &mut events, microphone),
            &mut call,
            session_options(),
            std::future::pending(),
            |notice| {
                reuse_notice(notice, &completed, &again);
                Ok(())
            },
        )
        .await
    };
    let remote = two_wakes(&mut server, &mut receive, &state);
    let mut work = Box::pin(async { tokio::join!(client, remote).0 });
    settle_reused_wakes(work.as_mut(), &state, &audio, &completed, &observer).await;
    drop(work);
    assert!(io.begin_request("account/read", &json!({})).is_ok());
}

async fn settle_reused_wakes(
    mut work: Pin<&mut impl Future<Output = Result<u64, Error>>>,
    state: &State,
    audio: &mpsc::Sender<PcmFrame>,
    completed: &Mutex<Vec<crate::bridge_voice::Stats>>,
    observer: &Observer,
) {
    settle_wake_reply(work.as_mut(), observer).await;
    assert_eq!(completed.lock().unwrap().len(), 1);
    tokio::time::advance(Duration::from_secs(4)).await;
    crate::bridge_voice::TEST_WAKE_TIME.with(|clock| clock.set(t0() + Duration::from_secs(420)));
    audio.send([123; 320]).await.unwrap(); // belongs to the prior, idle reply
    wake(observer);
    settle_wake_reply(work.as_mut(), observer).await;
    assert_eq!(completed.lock().unwrap().len(), 2);
    verify_renewed_idle(work, state).await;
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(*state.sent.lock().unwrap(), vec![[0; 320]; 2]);
    assert_no_reply(&completed.lock().unwrap(), observer);
}

async fn settle_wake_reply(mut work: Pin<&mut impl Future<Output = Result<u64, Error>>>, observer: &Observer) {
    pump(work.as_mut()).await;
    assert_eq!(observer.commands().last(), Some(&Command::Stop));
    observer.finish(Ok(()));
    pump(work.as_mut()).await;
}

async fn verify_renewed_idle(mut work: Pin<&mut impl Future<Output = Result<u64, Error>>>, state: &State) {
    tokio::time::advance(Duration::from_secs(4)).await;
    pump(work.as_mut()).await;
    assert_eq!(state.closes.load(Ordering::SeqCst), 0);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(work.await, Ok(2));
}

fn reuse_notice(
    notice: SessionNotice,
    completed: &Mutex<Vec<crate::bridge_voice::Stats>>,
    started: &mpsc::UnboundedSender<()>,
) {
    match notice {
        SessionNotice::Completed(outcome) => completed.lock().unwrap().push(outcome),
        SessionNotice::Started => started.send(()).unwrap(),
        _ => (),
    }
}

fn assert_refresh(request: &serde_json::Value, context: &str) {
    let text = format!(
        "{context}\nUse this newer time snapshot for time answers. This is quiet context. Do not read it aloud or reply to it."
    );
    assert_eq!(request["method"], "thread/realtime/appendText");
    assert_eq!(
        request["params"],
        json!({"threadId":"thread-1", "role":"developer", "text":text})
    );
    writeln!(std::io::stdout(), "observed retained-wake request: {request}").unwrap();
}

async fn start_call<R, W>(call: &mut MediaCall<'_, R, W, Controlled>)
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use crate::bridge_voice::{TEST_WAKE_TIME, call_instructions};
    let zone = Some(ZONE);
    let now = TEST_WAKE_TIME.with(std::cell::Cell::get);
    let base = call_instructions("Client audio bridge", now, zone).unwrap();
    let prompt = call_instructions("Live prompt", now, zone).unwrap();
    let options = crate::bridge_codex::Options {
        instructions: &base,
        prompt: &prompt,
        time_zone: zone,
        ..options()
    };
    call.start(&options, vec![], deadline()).await.unwrap();
}

async fn two_wakes(server: &mut Server, starts: &mut mpsc::UnboundedReceiver<()>, state: &State) {
    let params = start_with_prompt(server, INITIAL).await;
    assert_eq!(params["baseInstructions"], BASE);
    starts.recv().await.unwrap();
    reply(server).await;
    let request = tokio::select! {
        biased;
        request = read(server) => request,
        _ = starts.recv() => unreachable!("missing retained-wake append before the second run"),
    };
    assert_refresh(&request, LATER);
    assert_eq!(state.sent.lock().unwrap().len(), 1);
    write(server, json!({"id":request["id"],"result":{}})).await;
    starts.recv().await.unwrap();
    reply(server).await;
    stop(server).await;
}

#[derive(Clone, Copy)]
enum Failure {
    Refused,
    Realtime,
    Deadline,
    Cancelled,
}

#[tokio::test(start_paused = true)]
async fn voice_retained_wake_time_refresh_failure_keeps_capture_blocked_and_stops_call() {
    for failure in [
        Failure::Refused,
        Failure::Realtime,
        Failure::Deadline,
        Failure::Cancelled,
    ] {
        crate::bridge_voice::TEST_WAKE_TIME
            .scope(std::cell::Cell::new(t0()), failed(failure))
            .await;
    }
}

async fn failed(failure: Failure) {
    let (mut io, mut server) = fixture();
    let (peer, media, _audio) = peer(true, None);
    let (link, mut events, microphone, observer) = device().await;
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let (request, mut admitted) = mpsc::unbounded_channel();
    let counts = std::cell::Cell::new((0, 0));
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), vec![], deadline()).await.unwrap();
        conversation(
            (&link, &mut events, microphone),
            &mut call,
            session_options(),
            async {
                cancelled.await.unwrap();
            },
            |notice| {
                count(&counts, &notice);
                Ok(())
            },
        )
        .await
    };
    let remote = failed_request(&mut server, request, failure);
    let mut work = Box::pin(async { tokio::join!(client, remote).0 });
    pump(work.as_mut()).await;
    observer.finish(Ok(()));
    pump(work.as_mut()).await;
    assert_eq!(counts.get(), (1, 1));
    wake(&observer);
    let result = wait_failure(work.as_mut(), &mut admitted, cancel, failure).await;
    drop(work);
    assert_eq!(result, Err(expected_error(failure)));
    assert_eq!(counts.get(), (1, 1));
    assert_eq!(*media.sent.lock().unwrap(), vec![[0; 320]]);
    assert_eq!(media.closes.load(Ordering::SeqCst), 1);
    assert_available_for_next_call(&mut io, &mut server).await;
}

async fn wait_failure(
    mut work: Pin<&mut impl Future<Output = Result<u64, Error>>>,
    admitted: &mut mpsc::UnboundedReceiver<()>,
    cancel: tokio::sync::oneshot::Sender<()>,
    failure: Failure,
) -> Result<u64, Error> {
    // Keep the work driven even if refusal finishes immediately.
    tokio::select! {
        biased;
        result = &mut work => result,
        receipt = admitted.recv() => {
            receipt.unwrap();
            if matches!(failure, Failure::Cancelled) { cancel.send(()).unwrap(); }
            (&mut work).await
        }
    }
}

fn expected_error(failure: Failure) -> Error {
    match failure {
        Failure::Refused | Failure::Realtime => Error::Unavailable,
        Failure::Deadline => Error::Deadline,
        Failure::Cancelled => Error::Control,
    }
}

async fn assert_available_for_next_call(io: &mut Io, server: &mut Server) {
    let client = async {
        let mut call = crate::bridge_codex::Call::new(io);
        assert_eq!(
            call.start(&options(), vec![], "fixture-offer", deadline()).await,
            Ok("fixture-answer".into())
        );
        assert_eq!(call.stop(deadline()).await, Ok(()));
    };
    let remote = async {
        start(server).await;
        stop(server).await;
    };
    tokio::join!(client, remote);
}

fn count(counts: &std::cell::Cell<(usize, usize)>, notice: &SessionNotice) {
    let (starts, replies) = counts.get();
    match notice {
        SessionNotice::Started => counts.set((starts + 1, replies)),
        SessionNotice::ReplyCompleted(_) => counts.set((starts, replies + 1)),
        _ => (),
    }
}

async fn failed_request(server: &mut Server, admitted: mpsc::UnboundedSender<()>, failure: Failure) {
    start(server).await;
    reply(server).await;
    let item = read(server).await;
    assert_refresh(
        &item,
        "[Wake-time context: 2026-02-03 Tuesday 03:30:05 UTC+00:00; local offset unavailable]", // lint-rules: allow-literal synthetic UTC clock text
    );
    admitted.send(()).unwrap();
    refuse(server, &item, failure).await;
    stop(server).await;
}

async fn refuse(server: &mut Server, item: &serde_json::Value, failure: Failure) {
    match failure {
        Failure::Refused => {
            write(
                server,
                json!({"id":item["id"], "error":{"code":-32603, "message":"fixture refusal"}}),
            )
            .await;
        }
        Failure::Realtime => {
            notification(
                server,
                "thread/realtime/error",
                json!({"threadId":"thread-1", "message":"fixture error"}),
            )
            .await;
        }
        Failure::Deadline | Failure::Cancelled => (),
    }
}
