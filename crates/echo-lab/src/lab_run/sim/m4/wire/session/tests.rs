use super::*;
use crate::lab_run::sim::m4::wire::{Timing, frame};
use echo_contracts::wyoming::{Header, encode};
use tokio::{io::AsyncWriteExt, net::TcpListener};

fn at(milliseconds: u64) -> Duration {
    Duration::from_millis(milliseconds)
}

fn event(name: &str, bytes: usize) -> Event {
    Event {
        header: Header {
            r#type: name.into(),
            data: Some(json!({"rate":16000,"width":2,"channels":1})),
            data_length: 0,
            payload_length: bytes,
        },
        data: None,
        payload: vec![0; bytes],
    }
}

fn pcm(level: i16) -> Event {
    let mut value = event("audio-chunk", 960);
    value.header.data = Some(json!({"rate":24000,"width":2,"channels":1}));
    value.payload = level.to_le_bytes().repeat(480);
    value
}

fn reply(session: &mut Session, start: u64, level: i16) {
    session.playback(&event("audio-start", 0), at(start), false);
    session.playback(&pcm(level), at(start + 1), false);
    session.playback(&event("audio-stop", 0), at(start + 2), false);
}

fn check_reply(stream: &Value, start: u64, level: i16, played: Option<u64>) {
    assert_eq!(
        stream,
        &json!({"started_us":start * 1000,"stopped_us":(start + 2) * 1000,
        "played_us":played.map(|time| time * 1000),"ending":"stop","earcon":false,
        "pcm_bytes":960,"audible_samples":480,"minimum":level,"maximum":level})
    );
}

#[test]
fn two_reply_receipts_and_continued_capture_never_imply_run_end() {
    let mut session = Session::default();
    session.capture(&event("run-pipeline", 0), at(10));
    reply(&mut session, 20, 1200);
    session.capture(&event("played", 0), at(30));
    session.capture(&event("audio-chunk", 640), at(40));
    reply(&mut session, 50, -2400);
    session.capture(&event("played", 0), at(60));
    let observed = session.observation();
    assert_eq!(observed["run_opened_us"], json!([10000]));
    assert_eq!(observed["explicit_end_us"], json!([]));
    assert_eq!(observed["capture_chunks_after_reply_played"], 1);
    assert_eq!(observed["unmatched_played"], 0);
    assert_eq!(observed["streams"].as_array().unwrap().len(), 2);
    check_reply(&observed["streams"][0], 20, 1200, Some(30));
    check_reply(&observed["streams"][1], 50, -2400, Some(60));
    session.playback(&event("hassmic-end", 0), at(3060), false);
    assert_eq!(session.observation()["explicit_end_us"], json!([3_060_000]));
}

#[test]
fn cut_receipts_remain_ordered_and_missing_receipts_are_not_success() {
    let mut session = Session::default();
    session.playback(&event("audio-start", 0), at(10), false);
    session.playback(&pcm(900), at(11), true);
    session.playback(&event("hassmic-flush", 0), at(12), false);
    reply(&mut session, 20, -1200);
    session.capture(&event("played", 0), at(30));
    let observed = session.observation();
    assert_eq!(observed["streams"][0]["earcon"], true);
    assert_eq!(observed["streams"][0]["ending"], "flush");
    assert_eq!(observed["streams"][0]["played_us"], 30000);
    check_reply(&observed["streams"][1], 20, -1200, None);
    assert_eq!(observed["capture_chunks_after_reply_played"], 0);
    session.capture(&event("played", 0), at(40));
    session.capture(&event("played", 0), at(41));
    check_reply(&session.observation()["streams"][1], 20, -1200, Some(40));
    assert_eq!(session.observation()["unmatched_played"], 1);
}

// T3: the actual loopback relay forwards both replies and the explicit end.
// A write failure must not record an unforwarded frame as a device command.
#[tokio::test]
async fn t3_session_wire_observes_only_forwarded_commands() {
    tokio::time::timeout(Duration::from_secs(5), socket_journey())
        .await
        .unwrap();
}

async fn socket_journey() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let relay = crate::lab_run::sim::m4::wire::Relay::bind_reply(address).await.unwrap();
    let mut bridge = tokio::net::TcpStream::connect(relay.address).await.unwrap();
    let mut host = listener.accept().await.unwrap().0;
    exchange_run(&mut bridge, &mut host).await;
    let mut receipts = relay.observed.clone();
    receipts
        .wait_for(|timing| {
            timing.session.ended.len() == 1 && timing.session.streams.iter().all(|stream| stream.played.is_some())
        })
        .await
        .unwrap();
    check_observation(&relay.close().await.unwrap().session.observation());
}

async fn exchange_run(bridge: &mut tokio::net::TcpStream, host: &mut tokio::net::TcpStream) {
    let wake = event("run-pipeline", 0);
    host.write_all(&encode(&wake)).await.unwrap();
    assert_eq!(frame(bridge).await.unwrap().unwrap().1, wake);
    for level in [1200, -2400] {
        exchange_reply(bridge, host, level).await;
        let capture = event("audio-chunk", 640);
        host.write_all(&encode(&capture)).await.unwrap();
        assert_eq!(frame(bridge).await.unwrap().unwrap().1, capture);
    }
    let end = event("hassmic-end", 0);
    bridge.write_all(&encode(&end)).await.unwrap();
    assert_eq!(frame(host).await.unwrap().unwrap().1, end);
}

fn check_observation(observed: &Value) {
    assert_eq!(observed["run_opened_us"].as_array().unwrap().len(), 1);
    assert_eq!(observed["explicit_end_us"].as_array().unwrap().len(), 1);
    assert_eq!(observed["capture_chunks_after_reply_played"], 2);
    for (index, level) in [1200, -2400].into_iter().enumerate() {
        let stream = &observed["streams"][index];
        assert_eq!(stream["pcm_bytes"], 960);
        assert_eq!(stream["minimum"], level);
        assert_eq!(stream["maximum"], level);
        assert!(stream["played_us"].as_u64().unwrap() >= stream["stopped_us"].as_u64().unwrap());
    }
}

async fn exchange_reply(bridge: &mut tokio::net::TcpStream, host: &mut tokio::net::TcpStream, level: i16) {
    for value in [event("audio-start", 0), pcm(level), event("audio-stop", 0)] {
        bridge.write_all(&encode(&value)).await.unwrap();
        assert_eq!(frame(host).await.unwrap().unwrap().1, value);
    }
    let played = event("played", 0);
    host.write_all(&encode(&played)).await.unwrap();
    assert_eq!(frame(bridge).await.unwrap().unwrap().1, played);
}

#[tokio::test]
async fn session_failed_forward_cannot_fabricate_end_or_played() {
    let (observe, observed) = tokio::sync::watch::channel(Timing::default());
    let bytes = encode(&event("hassmic-end", 0));
    let (mut output, dropped) = tokio::io::duplex(64);
    drop(dropped);
    let result =
        crate::lab_run::sim::m4::wire::forward(&mut bytes.as_slice(), &mut output, &observe, |state, value| {
            state.playback(value, at(100))
        })
        .await;
    assert_eq!(result, Err("wire: forwarding failed".into()));
    assert_eq!(observed.borrow().session.observation()["explicit_end_us"], json!([]));
    assert_eq!(observed.borrow().session.observation()["streams"], json!([]));
}
