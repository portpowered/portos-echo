//! T1: a pending context append never releases the next wake on failure.
use super::*;

const ZONE: &str = "America/Los_Angeles";
const INITIAL: &str = "Live prompt\n[Call-start context: 2026-02-03 Tuesday 03:30:05 UTC+00:00; operator time zone: America/Los_Angeles; answer time questions in America/Los_Angeles]";
const BASE: &str = "Client audio bridge\n[Call-start context: 2026-02-03 Tuesday 03:30:05 UTC+00:00; operator time zone: America/Los_Angeles; answer time questions in America/Los_Angeles]";
const LATER: &str = "[Wake-time context: 2026-02-03 Tuesday 03:37:05 UTC+00:00; operator time zone: America/Los_Angeles; answer time questions in America/Los_Angeles]";

pub(super) fn t0() -> std::time::SystemTime {
    std::time::UNIX_EPOCH + Duration::from_secs(1_770_089_405)
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
    use std::io::Write;
    writeln!(std::io::stdout(), "observed retained-wake request: {request}").unwrap();
}

pub(super) async fn start_call<R, W>(call: &mut MediaCall<'_, R, W, Controlled>)
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

pub(super) async fn two_wakes(server: &mut Server, starts: &mut mpsc::UnboundedReceiver<()>, state: &State) {
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
                let (starts, replies) = counts.get();
                match notice {
                    SessionNotice::Started => counts.set((starts + 1, replies)),
                    SessionNotice::ReplyCompleted(_) => counts.set((starts, replies + 1)),
                    _ => (),
                }
                Ok(())
            },
        )
        .await
    };
    let remote = async {
        start(&mut server).await;
        reply(&mut server).await;
        let item = read(&mut server).await;
        assert_refresh(
            &item,
            "[Wake-time context: 2026-02-03 Tuesday 03:30:05 UTC+00:00; local offset unavailable]",
        );
        request.send(()).unwrap();
        refuse(&mut server, &item, failure).await;
        stop(&mut server).await;
    };
    let mut work = Box::pin(async { tokio::join!(client, remote).0 });
    pump(work.as_mut()).await;
    observer.finish(Ok(()));
    pump(work.as_mut()).await;
    assert_eq!(counts.get(), (1, 1));
    wake(&observer);
    // Keep the work driven even if refusal finishes immediately.
    let result = tokio::select! {
        biased;
        result = &mut work => result,
        receipt = admitted.recv() => {
            receipt.unwrap();
            if matches!(failure, Failure::Cancelled) { cancel.send(()).unwrap(); }
            work.await
        }
    };
    let expected = match failure {
        Failure::Refused | Failure::Realtime => Error::Unavailable,
        Failure::Deadline => Error::Deadline,
        Failure::Cancelled => Error::Control,
    };
    assert_eq!(result, Err(expected));
    assert_eq!(counts.get(), (1, 1));
    assert_eq!(*media.sent.lock().unwrap(), vec![[0; 320]]);
    assert_eq!(media.closes.load(Ordering::SeqCst), 1);
    assert!(io.begin_request("account/read", &json!({})).is_ok());
}

async fn refuse(server: &mut Server, item: &serde_json::Value, failure: Failure) {
    match failure {
        Failure::Refused => {
            write(
                server,
                json!({"id":item["id"], "error":{"code":-32603, "message":"fixture refusal"}}),
            )
            .await
        }
        Failure::Realtime => {
            notification(
                server,
                "thread/realtime/error",
                json!({"threadId":"thread-1", "message":"fixture error"}),
            )
            .await
        }
        Failure::Deadline | Failure::Cancelled => (),
    }
}
