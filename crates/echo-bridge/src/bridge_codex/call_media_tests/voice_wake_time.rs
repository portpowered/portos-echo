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
