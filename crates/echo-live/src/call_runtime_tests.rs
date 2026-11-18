//! T3: real HTTP and ICE/DTLS/SRTP/SCTP, only literal loopback, paused per-test time.
use crate::call::{
    CallError, CredentialProvider, LiveAuthMode, LiveCall, LiveCallFactory, LiveCredential, SessionOptions, now,
};
use crate::fake::{FakeLive, FakeResponse, FakeScript, ObservedCall};
use crate::{AudioFrame, Error, LivePeer};
use echo_contracts::ids::{DelegationId, SessionId};
use echo_contracts::live::LiveEvent;
use echo_contracts::secret::Secret;
use serde_json::json;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Weak;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

struct Credentials(LiveAuthMode);
impl CredentialProvider for Credentials {
    fn credential(&self) -> impl Future<Output = Result<LiveCredential, CallError>> + Send {
        std::future::ready(Ok(match self.0 {
            LiveAuthMode::Chatgpt => LiveCredential::Chatgpt {
                token: Secret::new("synthetic-token".into()),
                account_id: "synthetic-account".into(),
            },
            LiveAuthMode::ApiKey => LiveCredential::ApiKey {
                key: Secret::new("synthetic-token".into()),
            },
        }))
    }
}

fn script() -> FakeScript {
    FakeScript::from_json(include_str!("../../../tests/fixtures/live/session.synthetic.json")).unwrap()
}

fn options(mode: LiveAuthMode) -> SessionOptions {
    SessionOptions {
        model: match mode {
            LiveAuthMode::Chatgpt => "gpt-live-1-codex",
            LiveAuthMode::ApiKey => "gpt-live-1",
        }
        .into(),
        voice: "synthetic-voice".into(),
        instructions: "Reply ☃".into(),
        session_id: SessionId::new("synthetic-session").unwrap(),
        thread_id: "synthetic-thread".into(),
    }
}

async fn peer() -> LivePeer {
    Box::pin(LivePeer::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), now()))
        .await
        .unwrap()
}
async fn open(fake: &FakeLive, mode: LiveAuthMode) -> (LiveCall, Weak<UdpSocket>) {
    let peer = peer().await;
    let socket = peer.socket_owner();
    assert!(socket.upgrade().is_some());
    let factory = LiveCallFactory::new(&fake.base_url(), mode, Credentials(mode)).unwrap();
    let call = factory
        .create(peer, options(mode), now() + Duration::from_secs(10))
        .await
        .unwrap();
    (call, socket)
}

// T3: sequential calls deliver with paused time held by a runnable observer.
#[tokio::test(start_paused = true)]
async fn t3_sequential_calls_deliver_with_both_clocks_frozen() {
    let fake = FakeLive::bind(script()).await.unwrap();
    let tick = now();
    let ceiling = Instant::now() + Duration::from_secs(5);
    for _ in 0..2 {
        let mut operation = Box::pin(async {
            let (mut call, socket) = open(&fake, LiveAuthMode::ApiKey).await;
            replay_script(&mut call).await;
            call.close();
            assert!(socket.upgrade().is_none());
        });
        std::future::poll_fn(|cx| {
            assert!(Instant::now() < ceiling, "sequential frozen call stalled");
            cx.waker().wake_by_ref();
            operation.as_mut().poll(cx)
        })
        .await;
        assert_eq!(now(), tick);
    }
    assert_eq!(fake.observer().calls().unwrap().len(), 2);
    fake.shutdown().await;
}

async fn receive(call: &mut LiveCall) -> LiveEvent {
    call.receive_event(now() + Duration::from_secs(10)).await.unwrap()
}

async fn replay_script(call: &mut LiveCall) {
    for text in script().events {
        assert_eq!(receive(call).await, crate::call::decode(&text).unwrap());
    }
}

async fn exchange(call: &mut LiveCall, marker: u32, script: FakeScript) {
    let mut id = None;
    for text in script.events {
        let expected = crate::call::decode(&text).unwrap();
        assert_eq!(receive(call).await, expected);
        if let LiveEvent::DelegationCreated { delegation, .. } = expected {
            id = Some(delegation.id);
        }
    }
    let id = id.unwrap();
    call.append_commentary(id, "Reply ☃".into()).await.unwrap();
    assert_eq!(
        receive(call).await,
        LiveEvent::Other(json!({"type": "fake.commentary.received"}))
    );
    for sequence in 0..3 {
        call.send_audio(frame(sequence, marker), now()).await.unwrap();
        assert_eq!(
            receive(call).await,
            LiveEvent::Other(json!({"type": "fake.audio.frames", "count": sequence + 1}))
        );
        assert_eq!(
            call.receive_audio(now() + Duration::from_secs(5)).await.unwrap(),
            frame(sequence, marker)
        );
    }
    assert_eq!(call.dropped_audio(), 0);
}

fn frame(sequence: u16, marker: u32) -> AudioFrame {
    AudioFrame {
        sequence,
        timestamp: marker + u32::from(sequence) * 960,
        payload: vec![0xf8, 0xff, 0xfe],
    }
}

fn assert_request(call: &ObservedCall, mode: LiveAuthMode) {
    let request = &call.request;
    assert!(request.header_matches("authorization", "Bearer synthetic-token"));
    assert!(request.header_matches("user-agent", "portos-echo/0.1.0"));
    assert_eq!(
        request.session(),
        &json!({"model": options(mode).model, "voice": "synthetic-voice", "instructions": "Reply ☃", "delegation": {"target": "client"}})
    );
    match mode {
        LiveAuthMode::Chatgpt => {
            assert_eq!(
                request.path(),
                "/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas"
            );
            for (key, expected) in [
                ("openai-alpha", "quicksilver=v2"),
                ("originator", "portos-echo"),
                ("session-id", "synthetic-session"),
                ("x-session-id", "synthetic-session"),
                ("thread-id", "synthetic-thread"),
                ("chatgpt-account-id", "synthetic-account"),
            ] {
                assert!(request.header_matches(key, expected));
            }
        }
        LiveAuthMode::ApiKey => {
            assert_eq!(request.path(), "/v1/live/sessions");
            for name in [
                "chatgpt-account-id",
                "openai-alpha",
                "originator",
                "session-id",
                "x-session-id",
                "thread-id",
            ] {
                assert!(!request.has_header(name));
            }
        }
    }
}

async fn lifecycle(mode: LiveAuthMode) {
    let fake = FakeLive::bind(script()).await.unwrap();
    let (mut call, socket) = open(&fake, mode).await;
    exchange(&mut call, 10_000, script()).await;
    let observed = fake.observer().calls().unwrap();
    assert_eq!(observed.len(), 1);
    assert_request(&observed[0], mode);
    assert_eq!(
        observed[0].frames,
        (0..3).map(|seq| frame(seq, 10_000)).collect::<Vec<_>>()
    );
    assert_eq!(
        observed[0].commentary,
        [r#"{"type":"session.commentary.append","delegation_id":"synthetic-delegation-1","text":"Reply ☃"}"#]
    );
    assert_closed(&mut call, socket).await;
    print_summary(mode, observed[0].request.path());
    let records = fake.observer();
    fake.shutdown().await;
    assert!(records.calls().unwrap()[0].closed);
}

async fn assert_closed(call: &mut LiveCall, socket: Weak<UdpSocket>) {
    call.close();
    call.close();
    assert_eq!(call.receive_event(now()).await, Err(Error::Closed.into()));
    assert_eq!(
        call.append_commentary(DelegationId::new("d").unwrap(), "x".into())
            .await,
        Err(Error::Closed.into())
    );
    assert_eq!(call.send_audio(frame(0, 0), now()).await, Err(Error::Closed.into()));
    assert_eq!(call.pop_audio(), None);
    assert!(socket.upgrade().is_none());
}

fn print_summary(mode: LiveAuthMode, path: &str) {
    use std::io::Write;
    writeln!(std::io::stdout(), "mode={mode:?} path={path} authorization_matches=true model={} delegation_target=client delegation_id=synthetic-delegation-1 commentary_count=1 event_order=session.created,session.delegation.created,future.event,error audio_counts=1,2,3 frames=3 drops=0 closed=true", options(mode).model).unwrap();
    match mode {
        LiveAuthMode::Chatgpt => writeln!(std::io::stdout(), "OpenAI-Alpha=quicksilver=v2 originator=portos-echo User-Agent=portos-echo/0.1.0 session-id=synthetic-session x-session-id=synthetic-session thread-id=synthetic-thread chatgpt-account-id_matches=true").unwrap(),
        LiveAuthMode::ApiKey => writeln!(std::io::stdout(), "query empty chatgpt_only_headers=0 User-Agent=portos-echo/0.1.0").unwrap(),
    }
}

#[tokio::test(start_paused = true)]
async fn t3_call_chatgpt_lifecycle() {
    lifecycle(LiveAuthMode::Chatgpt).await;
}

#[tokio::test(start_paused = true)]
async fn t3_call_api_key_lifecycle() {
    lifecycle(LiveAuthMode::ApiKey).await;
}

async fn fails(response: FakeResponse, expected: CallError) {
    let mut script = script();
    script.response = response;
    let fake = FakeLive::bind(script).await.unwrap();
    for mode in [LiveAuthMode::Chatgpt, LiveAuthMode::ApiKey] {
        let peer = peer().await;
        let socket = peer.socket_owner();
        assert!(socket.upgrade().is_some());
        let factory = LiveCallFactory::new(&fake.base_url(), mode, Credentials(mode)).unwrap();
        let result = factory
            .create(peer, options(mode), now() + Duration::from_secs(2))
            .await;
        assert_eq!(result.err().as_ref(), Some(&expected));
        assert!(socket.upgrade().is_none());
    }
    assert_eq!(fake.observer().calls().unwrap().len(), 2);
    fake.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn t3_call_authorization_outage_invalid_sdp_disconnect() {
    for status in [401, 403] {
        fails(
            FakeResponse::Http {
                status,
                body: "synthetic-token private remote input".into(),
                location: None,
            },
            CallError::Authorization,
        )
        .await;
    }
    fails(
        FakeResponse::Http {
            status: 500,
            body: "private provider input".into(),
            location: None,
        },
        CallError::Unavailable,
    )
    .await;
    for body in ["", "invalid private sdp"] {
        fails(FakeResponse::InvalidSdp(body.into()), Error::InvalidSdp.into()).await;
    }
    fails(FakeResponse::Disconnect, CallError::Signaling).await;
}

#[tokio::test(start_paused = true)]
async fn t3_call_deadlines_release_peer() {
    fails(FakeResponse::StallHttp, CallError::Deadline).await;
    fails(FakeResponse::StallPeer, CallError::Deadline).await;
}

#[tokio::test(start_paused = true)]
async fn t3_call_malformed_event_closes_without_commentary() {
    for event in ["malformed private json", r#"{"type":"session.delegation.created"}"#] {
        let mut script = script();
        script.events = vec![event.into()];
        let fake = FakeLive::bind(script).await.unwrap();
        let (mut call, socket) = open(&fake, LiveAuthMode::ApiKey).await;
        assert_eq!(
            call.receive_event(now() + Duration::from_secs(5)).await,
            Err(CallError::Decode)
        );
        call.close();
        assert!(socket.upgrade().is_none());
        assert_eq!(fake.observer().calls().unwrap()[0].commentary.len(), 0);
        fake.shutdown().await;
    }
}

#[tokio::test(start_paused = true)]
async fn t3_call_redirect_never_sends_credentials() {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let mut redirect = reqwest::Url::parse("http://127.0.0.1").unwrap();
    redirect.set_port(Some(listener.local_addr().unwrap().port())).unwrap();
    redirect.set_path("/stolen");
    fails(
        FakeResponse::Http {
            status: 307,
            body: "redirect".into(),
            location: Some(redirect.to_string()),
        },
        CallError::Signaling,
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(1), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn t3_call_concurrent_pairs_remain_isolated() {
    let fake_a = FakeLive::bind(script()).await.unwrap();
    let mut second = script();
    second.events = second
        .events
        .into_iter()
        .map(|event| event.replace("synthetic-delegation-1", "synthetic-delegation-2"))
        .collect();
    let fake_b = FakeLive::bind(second.clone()).await.unwrap();
    let ((mut a, _), (mut b, _)) = tokio::join!(
        open(&fake_a, LiveAuthMode::Chatgpt),
        open(&fake_b, LiveAuthMode::ApiKey)
    );
    tokio::join!(exchange(&mut a, 10_000, script()), exchange(&mut b, 20_000, second));
    a.close();
    b.append_commentary(DelegationId::new("still-b").unwrap(), "still open".into())
        .await
        .unwrap();
    assert_eq!(
        receive(&mut b).await,
        LiveEvent::Other(json!({"type": "fake.commentary.received"}))
    );
    assert_eq!(
        fake_a.observer().calls().unwrap()[0].frames,
        (0..3).map(|seq| frame(seq, 10_000)).collect::<Vec<_>>()
    );
    let observed = fake_b.observer().calls().unwrap();
    assert_eq!(
        observed[0].frames,
        (0..3).map(|seq| frame(seq, 20_000)).collect::<Vec<_>>()
    );
    assert_eq!(observed[0].commentary.len(), 2);
    b.close();
    fake_a.shutdown().await;
    fake_b.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn t3_call_oversize_commentary_rejected_without_send() {
    let fake = FakeLive::bind(script()).await.unwrap();
    let (mut call, _) = open(&fake, LiveAuthMode::ApiKey).await;
    replay_script(&mut call).await;
    let id = DelegationId::new("d").unwrap();
    assert_eq!(
        call.append_commentary(id.clone(), "a".repeat(16_384)).await,
        Err(Error::Full.into())
    );
    call.append_commentary(id, "accepted".into()).await.unwrap();
    assert_eq!(
        receive(&mut call).await,
        LiveEvent::Other(json!({"type": "fake.commentary.received"}))
    );
    assert_eq!(fake.observer().calls().unwrap()[0].commentary.len(), 1);
    call.close();
    fake.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn t3_call_cancel_create_releases_peer_without_retry() {
    for response in [FakeResponse::StallHttp, FakeResponse::StallPeer] {
        let mut script = script();
        script.response = response;
        let fake = FakeLive::bind(script).await.unwrap();
        let peer = peer().await;
        let socket = peer.socket_owner();
        assert!(socket.upgrade().is_some());
        let factory = LiveCallFactory::new(
            &fake.base_url(),
            LiveAuthMode::ApiKey,
            Credentials(LiveAuthMode::ApiKey),
        )
        .unwrap();
        let mut attempt =
            Box::pin(factory.create(peer, options(LiveAuthMode::ApiKey), now() + Duration::from_secs(10)));
        let observer = fake.observer();
        tokio::select! {
            result = &mut attempt => assert!(result.err() == Some(CallError::Deadline)),
            result = observer.wait_for_calls(1) => result.unwrap(),
        }
        drop(attempt);
        assert!(socket.upgrade().is_none());
        assert_eq!(observer.calls().unwrap().len(), 1);
        fake.shutdown().await;
    }
}

#[tokio::test(start_paused = true)]
async fn t3_call_receive_pressure_preserves_accepted_fifo() {
    let mut script = script();
    script.events = (0..17)
        .map(|index| json!({"type": "future.event", "index": index}).to_string())
        .collect();
    let fake = FakeLive::bind(script).await.unwrap();
    let (mut call, _) = open(&fake, LiveAuthMode::ApiKey).await;
    assert_eq!(
        call.receive_audio(now() + Duration::from_secs(10)).await,
        Err(Error::Full.into())
    );
    for index in 0..16 {
        assert_eq!(
            receive(&mut call).await,
            LiveEvent::Other(json!({"type": "future.event", "index": index}))
        );
    }
    call.close();
    assert_eq!(call.receive_audio(now()).await, Err(Error::Closed.into()));
    assert_eq!(fake.observer().calls().unwrap()[0].commentary.len(), 0);
    fake.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn t3_call_responses_target_is_observed_but_not_selected() {
    let mut script = script();
    script.events[1] = json!({"type": "session.delegation.created", "delegation": {"id": "responses-id", "target": "responses", "extra": 5}}).to_string();
    let fake = FakeLive::bind(script.clone()).await.unwrap();
    let (mut call, _) = open(&fake, LiveAuthMode::ApiKey).await;
    for event in script.events {
        assert_eq!(receive(&mut call).await, crate::call::decode(&event).unwrap());
    }
    call.append_commentary(DelegationId::new("supplied-client-id").unwrap(), "client result".into())
        .await
        .unwrap();
    assert_eq!(
        receive(&mut call).await,
        LiveEvent::Other(json!({"type": "fake.commentary.received"}))
    );
    let observed = fake.observer().calls().unwrap();
    assert_eq!(observed[0].request.session()["delegation"]["target"], "client");
    assert_eq!(
        crate::call::decode(&observed[0].commentary[0]).unwrap(),
        LiveEvent::CommentaryAppend {
            delegation_id: DelegationId::new("supplied-client-id").unwrap(),
            text: "client result".into(),
            extra: std::collections::BTreeMap::new()
        }
    );
    call.close();
    fake.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn t3_call_preflight_failures_release_peer_without_http() {
    struct Unavailable(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl CredentialProvider for Unavailable {
        fn credential(&self) -> impl Future<Output = Result<LiveCredential, CallError>> + Send {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::ready(Err(CallError::Authorization))
        }
    }
    let fake = FakeLive::bind(script()).await.unwrap();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let provider = Unavailable(count.clone());
    let factory = LiveCallFactory::new(&fake.base_url(), LiveAuthMode::ApiKey, provider).unwrap();
    for (expired, invalid, expected) in [
        (true, false, CallError::Deadline),
        (false, true, CallError::Configuration),
        (false, false, CallError::Authorization),
    ] {
        let peer = peer().await;
        let socket = peer.socket_owner();
        assert!(socket.upgrade().is_some());
        let mut opts = options(LiveAuthMode::ApiKey);
        if invalid {
            opts.model = "wrong".into();
        }
        let deadline = if expired { now() } else { now() + Duration::from_secs(1) };
        assert_eq!(factory.create(peer, opts, deadline).await.err(), Some(expected));
        assert!(socket.upgrade().is_none());
    }
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(fake.observer().calls().unwrap().len(), 0);
    fake.shutdown().await;
}
