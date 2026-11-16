//! T3: real loopback HTTP/WebRTC, no provider or audio device.
use echo_contracts::ids::{DelegationId, SessionId};
use echo_contracts::live::LiveEvent;
use echo_contracts::secret::Secret;
use tokio::io::AsyncReadExt;

use super::*;
use crate::call::{CredentialProvider, LiveAuthMode, LiveCall, LiveCallFactory, LiveCredential, SessionOptions};

struct Credentials;
impl CredentialProvider for Credentials {
    fn credential(&self) -> impl Future<Output = Result<LiveCredential, CallError>> + Send {
        std::future::ready(Ok(LiveCredential::ApiKey {
            key: Secret::new("synthetic-token".into()),
        }))
    }
}

async fn open(fake: &DialogueLive) -> LiveCall {
    let peer = Box::pin(LivePeer::bind((Ipv4Addr::LOCALHOST, 0).into(), now()))
        .await
        .unwrap();
    let factory = LiveCallFactory::new(fake.base_url(), LiveAuthMode::ApiKey, Credentials).unwrap();
    factory
        .create(
            peer,
            SessionOptions {
                model: "gpt-live-1".into(),
                voice: "synthetic-voice".into(),
                instructions: "synthetic".into(),
                session_id: SessionId::new("synthetic-session").unwrap(),
                thread_id: "synthetic-thread".into(),
            },
            now() + Duration::from_secs(10),
        )
        .await
        .unwrap()
}

async fn receive(call: &mut LiveCall) -> LiveEvent {
    call.receive_event(now() + Duration::from_secs(10)).await.unwrap()
}

async fn wait(observer: &DialogueObserver, index: usize, predicate: impl Fn(&DialogueCall) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), observer.wait_for(index, predicate))
        .await
        .unwrap()
        .unwrap();
}

async fn append(call: &mut LiveCall, id: &str, text: &str) {
    call.append_commentary(DelegationId::new(id).unwrap(), text.into())
        .await
        .unwrap();
}

async fn receive_reply(call: &mut LiveCall, script: &DialogueScript) -> Vec<DialogueSent> {
    assert_eq!(
        receive(call).await,
        LiveEvent::Other(json!({"type":"response.audio_transcript.done", "transcript":script.transcript}))
    );
    assert_eq!(
        receive(call).await,
        LiveEvent::Other(json!({"type":"response.audio.done"}))
    );
    let expected = Dialogue::new(script.clone()).reply();
    for frame in expected.iter().filter_map(|item| match item {
        DialogueSent::Audio(frame) => Some(frame),
        DialogueSent::Text(_) => None,
    }) {
        assert_eq!(
            call.receive_audio(now() + Duration::from_secs(10)).await.unwrap(),
            *frame
        );
    }
    expected
}

#[tokio::test(start_paused = true)]
async fn t3_dialogue_waits_for_match_sends_exact_reply_once_and_reaps() {
    let script = script();
    let fake = crate::fake::FakeLive::bind_dialogue(script.clone()).await.unwrap();
    let observer = fake.observer();
    let mut call = open(&fake).await;
    assert_eq!(
        receive(&mut call).await,
        crate::call::decode(
            &json!({
                "type":"session.delegation.created", "delegation":{"id":"d-1","target":"client","input":script.input}
            })
            .to_string()
        )
        .unwrap()
    );
    append(&mut call, "wrong-id", &script.commentary).await;
    wait(&observer, 0, |call| call.rejected_commentary == 1).await;
    assert_eq!(observer.calls().unwrap()[0].sent.len(), 1);
    append(&mut call, "d-1", "wrong text").await;
    wait(&observer, 0, |call| call.rejected_commentary == 2).await;
    assert_eq!(observer.calls().unwrap()[0].sent.len(), 1);
    append(&mut call, "d-1", &script.commentary).await;
    let expected = receive_reply(&mut call, &script).await;
    append(&mut call, "d-1", &script.commentary).await;
    wait(&observer, 0, |call| call.rejected_commentary == 3).await;
    let sent = observer.calls().unwrap()[0].sent.clone();
    assert_eq!(&sent[1..], expected);
    assert_eq!((call.pop_audio(), call.dropped_audio()), (None, 0));
    call.close();
    let address = fake.base_url().strip_prefix("http://").unwrap().to_owned();
    fake.shutdown().await.unwrap();
    assert_eq!(observer.calls().unwrap()[0].call_index, 0);
    assert_eq!(observer.calls().unwrap()[0].delegation_id, "d-1");
    assert!(observer.calls().unwrap()[0].closed);
    assert!(observer.calls().unwrap()[0].reply_completed);
    assert_eq!(observer.calls().unwrap()[0].failure, None);
    assert_eq!(observer.calls().unwrap()[0].sent, sent);
    assert!(TcpStream::connect(address).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn t3_dialogue_negative_is_fresh_and_has_its_own_error_transcript() {
    let mut script = script();
    script.commentary = "I couldn't call test echo.".into();
    script.transcript = script.commentary.clone();
    let fake = crate::fake::FakeLive::bind_dialogue(script.clone()).await.unwrap();
    let observer = fake.observer();
    let mut call = open(&fake).await;
    assert!(matches!(receive(&mut call).await, LiveEvent::DelegationCreated { .. }));
    append(&mut call, "d-1", &script.commentary).await;
    assert_eq!(
        receive(&mut call).await,
        LiveEvent::Other(json!({
            "type":"response.audio_transcript.done", "transcript":"I couldn't call test echo."
        }))
    );
    assert_eq!(
        receive(&mut call).await,
        LiveEvent::Other(json!({"type":"response.audio.done"}))
    );
    assert_eq!(observer.calls().unwrap().len(), 1);
    assert_eq!(observer.calls().unwrap()[0].rejected_commentary, 0);
    call.close();
    fake.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn t3_dialogue_partial_http_and_shutdown_collect_all_admitted_connections() {
    let fake = crate::fake::FakeLive::bind_dialogue(script()).await.unwrap();
    let address = fake.base_url().strip_prefix("http://").unwrap();
    let observer = fake.observer();
    let mut sockets = vec![];
    for index in 0..MAX_CALLS {
        sockets.push(TcpStream::connect(address).await.unwrap());
        wait(&observer, index, |call| !call.closed).await;
    }
    let mut excess = TcpStream::connect(address).await.unwrap();
    assert_eq!(
        excess.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    fake.shutdown().await.unwrap();
    for mut socket in sockets {
        assert_eq!(
            socket.read_u8().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
    let calls = observer.calls().unwrap();
    assert_eq!(calls.len(), MAX_CALLS);
    for call in calls {
        assert_eq!(call.sent, vec![]);
        assert_eq!(call.failure.as_deref(), Some("cancelled"));
        assert!(!call.reply_completed);
        assert!(call.closed);
    }
}

#[tokio::test(start_paused = true)]
async fn t3_dialogue_malformed_and_overflowing_commentary_never_send_reply() {
    let fake = crate::fake::FakeLive::bind_dialogue(script()).await.unwrap();
    let observer = fake.observer();
    let mut peer = Box::pin(LivePeer::bind((Ipv4Addr::LOCALHOST, 0).into(), now()))
        .await
        .unwrap();
    let offer = peer.offer().await.unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .post(format!("{}/v1/live/sessions", fake.base_url()))
        .json(&json!({"sdp":offer}))
        .send()
        .await
        .unwrap();
    peer.apply_answer(&response.text().await.unwrap()).await.unwrap();
    let deadline = now() + Duration::from_secs(10);
    while peer.pop_text().is_none() {
        drive(&mut peer, deadline).await.unwrap();
    }
    for index in 1..=64 {
        peer.send_text("{malformed").await.unwrap();
        wait(&observer, 0, |call| call.rejected_commentary == index).await;
        assert_eq!(observer.calls().unwrap()[0].sent.len(), 1);
    }
    peer.send_text(&commentary("d-1", &script().commentary)).await.unwrap();
    wait(&observer, 0, |call| call.closed).await;
    let calls = observer.calls().unwrap();
    assert_eq!(calls[0].sent.len(), 1);
    assert_eq!(calls[0].failure.as_deref(), Some("invalid Live event"));
    assert_eq!(calls[0].rejected_commentary, 64);
    peer.close();
    fake.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn t3_dialogue_partial_http_deadline_records_failure_and_fresh_call_recovers() {
    let fake = crate::fake::FakeLive::bind_dialogue(script()).await.unwrap();
    let observer = fake.observer();
    let mut socket = TcpStream::connect(fake.base_url().strip_prefix("http://").unwrap())
        .await
        .unwrap();
    wait(&observer, 0, |call| !call.closed).await;
    tokio::time::advance(Duration::from_secs(21)).await;
    wait(&observer, 0, |call| call.closed).await;
    assert_eq!(
        observer.calls().unwrap()[0].failure.as_deref(),
        Some("Live deadline reached")
    );
    assert_eq!(observer.calls().unwrap()[0].sent, vec![]);
    assert_eq!(
        socket.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    let mut call = open(&fake).await;
    assert!(matches!(receive(&mut call).await, LiveEvent::DelegationCreated { .. }));
    append(&mut call, "d-1", &script().commentary).await;
    receive_reply(&mut call, &script()).await;
    wait(&observer, 1, |call| call.sent.len() == 6).await;
    assert_eq!(observer.calls().unwrap()[1].failure, None);
    assert_eq!(observer.calls().unwrap()[1].call_index, 1);
    call.close();
    fake.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn t3_dialogue_call_identity_prevents_borrowing_another_calls_match() {
    let fake = crate::fake::FakeLive::bind_dialogue(script()).await.unwrap();
    let observer = fake.observer();
    let mut first = open(&fake).await;
    let mut second = open(&fake).await;
    for call in [&mut first, &mut second] {
        assert!(matches!(receive(call).await, LiveEvent::DelegationCreated { .. }));
    }
    append(&mut first, "d-1", &script().commentary).await;
    receive_reply(&mut first, &script()).await;
    wait(&observer, 0, |call| call.sent.len() == 6).await;
    assert_eq!(observer.calls().unwrap()[1].sent.len(), 1);
    append(&mut second, "wrong-id", &script().commentary).await;
    wait(&observer, 1, |call| call.rejected_commentary == 1).await;
    assert_eq!(observer.calls().unwrap()[1].sent.len(), 1);
    append(&mut second, "d-1", &script().commentary).await;
    receive_reply(&mut second, &script()).await;
    wait(&observer, 1, |call| call.sent.len() == 6).await;
    let calls = observer.calls().unwrap();
    assert_eq!((calls[0].call_index, calls[1].call_index), (0, 1));
    assert_eq!((calls[0].rejected_commentary, calls[1].rejected_commentary), (0, 1));
    assert_eq!(calls[0].sent, calls[1].sent);
    first.close();
    second.close();
    fake.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn t3_dialogue_drop_closes_listener_and_partial_http_tasks() {
    let fake = crate::fake::FakeLive::bind_dialogue(script()).await.unwrap();
    let address = fake.base_url().strip_prefix("http://").unwrap().to_owned();
    let mut socket = TcpStream::connect(&address).await.unwrap();
    wait(&fake.observer(), 0, |call| !call.closed).await;
    drop(fake);
    assert_eq!(
        socket.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    assert!(TcpStream::connect(address).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn t3_dialogue_initial_delegation_waits_for_explicit_capture_gate() {
    let mut script = script();
    script.initial_after_audio_frames = 1;
    let fake = crate::fake::FakeLive::bind_dialogue(script.clone()).await.unwrap();
    let observer = fake.observer();
    let mut call = open(&fake).await;
    assert_eq!(observer.calls().unwrap()[0].sent, vec![]);
    call.send_audio(
        AudioFrame {
            sequence: 0,
            timestamp: 0,
            payload: vec![0xf8, 0xff, 0xfe],
        },
        now(),
    )
    .await
    .unwrap();
    let event = receive(&mut call).await;
    assert!(matches!(event, LiveEvent::DelegationCreated { .. }));
    wait(&observer, 0, |call| call.received_audio == 1).await;
    append(&mut call, "d-1", &script.commentary).await;
    receive_reply(&mut call, &script).await;
    call.close();
    fake.shutdown().await.unwrap();
}
