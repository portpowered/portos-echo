//! T3 component tests: authenticated loopback transport and private fault injection.
use super::*;
use rtc::data_channel::RTCDataChannelMessage;
use rtc::peer_connection::message::TaggedRTCMessage;

fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}
async fn peer() -> LivePeer {
    LivePeer::bind(([127, 0, 0, 1], 0).into(), now()).await.unwrap()
}
fn frame(n: u16, len: usize) -> AudioFrame {
    AudioFrame {
        sequence: n,
        timestamp: u32::from(n) * 960,
        payload: vec![42; len],
    }
}
async fn step(a: &mut LivePeer, b: &mut LivePeer) -> Result<(), Error> {
    let next = a.flush().await?.min(b.flush().await?);
    let deadline = now() + Duration::from_secs(10);
    let selected = tokio::select! {
        r = a.readable() => { r?; 0 }
        r = b.readable() => { r?; 1 }
        () = tokio::time::sleep_until(next.min(deadline).into()) => 2,
    };
    if now() >= deadline {
        return Err(Error::Deadline);
    }
    match selected {
        0 => {
            a.receive(now()).await?;
        }
        1 => {
            b.receive(now()).await?;
        }
        _ => {
            a.timeout(now()).await?;
            b.timeout(now()).await?;
        }
    }
    Ok(())
}
async fn pair() -> (LivePeer, LivePeer) {
    let mut a = peer().await;
    let mut b = peer().await;
    let offer = a.offer().await.unwrap();
    assert!(!a.ready());
    let answer = b.answer(&offer).await.unwrap();
    assert!(!b.ready());
    a.apply_answer(&answer).await.unwrap();
    let deadline = now() + Duration::from_secs(10);
    while !a.ready() || !b.ready() {
        assert!(now() < deadline);
        step(&mut a, &mut b).await.unwrap();
    }
    (a, b)
}
async fn text_arrives(a: &mut LivePeer, b: &mut LivePeer) {
    let deadline = now() + Duration::from_secs(10);
    while b.text.items_empty() {
        assert!(now() < deadline);
        step(a, b).await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn t3_negotiation_errors_preserve_state_and_precedence() {
    let mut a = peer().await;
    let mut b = peer().await;
    assert_eq!(a.apply_answer("private").await, Err(Error::State));
    assert_eq!(a.answer("private").await, Err(Error::InvalidSdp));
    assert_eq!(a.send_audio(frame(0, 0), now()).await, Err(Error::NotReady));
    assert_eq!(a.send_text(&"x".repeat(16385)).await, Err(Error::NotReady));
    let incomplete = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n";
    assert_eq!(a.answer(incomplete).await, Err(Error::InvalidSdp));
    let offer = a.offer().await.unwrap();
    assert_eq!(a.offer().await, Err(Error::State));
    assert_eq!(a.answer("private").await, Err(Error::State));
    assert_eq!(a.apply_answer("private").await, Err(Error::InvalidSdp));
    assert_eq!(a.apply_answer(incomplete).await, Err(Error::InvalidSdp));
    let answer = b.answer(&offer).await.unwrap();
    assert_eq!(b.offer().await, Err(Error::State));
    assert_eq!(b.answer(&offer).await, Err(Error::State));
    a.apply_answer(&answer).await.unwrap();
    assert_eq!(a.apply_answer(&answer).await, Err(Error::State));
    a.close();
    assert_eq!(a.offer().await, Err(Error::Closed));
    assert_eq!(a.answer("private").await, Err(Error::Closed));
    assert_eq!(a.apply_answer("private").await, Err(Error::Closed));
    assert_eq!(a.flush().await, Err(Error::Closed));
}

#[tokio::test(start_paused = true)]
async fn t3_payload_edges_arrive_exactly_and_invalid_sends_do_not_arrive() {
    let (mut a, mut b) = Box::pin(pair()).await;
    for (n, len) in [(0, 1), (1, 1200)] {
        a.send_audio(frame(n, len), now()).await.unwrap();
        while b.audio.items_empty() {
            step(&mut a, &mut b).await.unwrap();
        }
        assert_eq!(b.pop_audio(), Some(frame(n, len)));
    }
    for text in [String::new(), "\u{e9}".repeat(8192)] {
        a.send_text(&text).await.unwrap();
        text_arrives(&mut a, &mut b).await;
        assert_eq!(b.pop_text(), Some(text));
    }
    for len in [0, 1201] {
        assert_eq!(a.send_audio(frame(2, len), now()).await, Err(Error::Payload));
    }
    assert_eq!(
        a.send_text(&format!("{}x", "\u{e9}".repeat(8192))).await,
        Err(Error::Full)
    );
    a.send_text("barrier").await.unwrap();
    text_arrives(&mut a, &mut b).await;
    assert_eq!(b.pop_text().as_deref(), Some("barrier"));
    assert_eq!(b.pop_audio(), None);
    assert_eq!(b.pop_text(), None);
}

#[tokio::test(start_paused = true)]
async fn t3_receive_pressure_retains_exact_fifo_and_drops_one_audio() {
    let (mut a, mut b) = Box::pin(pair()).await;
    for n in 0..129 {
        a.send_audio(frame(n, 1), now()).await.unwrap();
        let before = b.audio.items_len();
        while b.audio.items_len() == before && b.dropped_audio() == 0 {
            step(&mut a, &mut b).await.unwrap();
        }
    }
    assert_eq!(b.dropped_audio(), 1);
    for n in 1..129 {
        assert_eq!(b.pop_audio(), Some(frame(n, 1)));
    }
    assert_eq!(b.pop_audio(), None);
    for n in 0..16 {
        a.send_text(&n.to_string()).await.unwrap();
        while b.text.items_len() != n + 1 {
            step(&mut a, &mut b).await.unwrap();
        }
    }
    a.send_text("overflow").await.unwrap();
    let deadline = now() + Duration::from_secs(10);
    loop {
        assert!(now() < deadline);
        if let Err(error) = step(&mut a, &mut b).await {
            assert_eq!(error, Error::Full);
            break;
        }
    }
    b.close();
    assert!(!b.ready());
    for n in 0..16 {
        assert_eq!(b.pop_text(), Some(n.to_string()));
    }
    assert_eq!(b.pop_text(), None);
}

async fn bad_message(a: &mut LivePeer, b: &mut LivePeer, is_string: bool) {
    let id = a.channel.unwrap();
    let mut message = RTCMessage::DataChannelMessage(id, RTCDataChannelMessage::default());
    if let RTCMessage::DataChannelMessage(_, data) = &mut message {
        data.is_string = is_string;
        data.data = (&[0xff, 0xfe][..]).into();
    }
    a.rtc.handle_write(TaggedRTCMessage { now: now(), message }).unwrap();
    let deadline = now() + Duration::from_secs(10);
    loop {
        assert!(now() < deadline);
        if let Err(error) = step(a, b).await {
            assert_eq!(error, Error::Payload);
            break;
        }
    }
    assert_eq!((b.pop_audio(), b.pop_text()), (None, None));
}

#[tokio::test(start_paused = true)]
async fn t3_authenticated_binary_invalid_utf8_and_channel_close() {
    let (mut a, mut b) = Box::pin(pair()).await;
    bad_message(&mut a, &mut b, false).await;
    bad_message(&mut a, &mut b, true).await;
    a.rtc.data_channel(a.channel.unwrap()).unwrap().close().unwrap();
    let deadline = now() + Duration::from_secs(10);
    while b.ready() {
        assert!(now() < deadline);
        step(&mut a, &mut b).await.unwrap();
    }
    assert_eq!(b.send_text("closed channel").await, Err(Error::NotReady));
}

#[tokio::test(start_paused = true)]
async fn t3_drop_occupied_bind_and_queued_close() {
    let (mut a, mut b) = Box::pin(pair()).await;
    let address = b.local_addr();
    assert!(matches!(LivePeer::bind(address, now()).await, Err(Error::Io)));
    a.send_audio(frame(1, 1), now()).await.unwrap();
    a.send_text("queued").await.unwrap();
    while b.audio.items_empty() || b.text.items_empty() {
        step(&mut a, &mut b).await.unwrap();
    }
    let socket = b.socket_owner();
    assert!(socket.upgrade().is_some());
    b.close();
    b.close();
    assert_eq!(b.pop_audio(), Some(frame(1, 1)));
    assert_eq!(b.pop_text().as_deref(), Some("queued"));
    assert_eq!((b.pop_audio(), b.pop_text()), (None, None));
    assert!(socket.upgrade().is_none());
    // Drop must release an open peer too, independently of the closed peer.
    let other = peer().await;
    let socket = other.socket_owner();
    assert!(socket.upgrade().is_some());
    drop(other);
    assert!(socket.upgrade().is_none());
    assert!(a.socket_owner().upgrade().is_some());
}

#[tokio::test(start_paused = true)]
async fn t3_malformed_udp_is_sanitized_and_does_not_fabricate_data() {
    let mut a = peer().await;
    let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sender
        .send_to(b"private invalid datagram", a.local_addr())
        .await
        .unwrap();
    a.readable().await.unwrap();
    assert_eq!(a.receive(now()).await, Err(Error::Protocol));
    assert_eq!((a.pop_audio(), a.pop_text()), (None, None));
    assert!(!a.ready());
    assert_eq!(Error::Protocol.to_string(), "WebRTC protocol error");
}

#[test]
fn payload_admission_uses_exact_byte_bounds() {
    for n in [1, 1200] {
        assert_eq!(audio_payload(n), Ok(()));
    }
    for n in [0, 1201] {
        assert_eq!(audio_payload(n), Err(Error::Payload));
    }
    for n in [0, 16384] {
        assert_eq!(text_payload(n), Ok(()));
    }
    assert_eq!(text_payload(16385), Err(Error::Full));
    assert_eq!(datagram(&[]), Err(Error::Protocol));
    assert_eq!(datagram(&[0; 20]), Err(Error::Protocol));
    assert_eq!(datagram(&[128, 0]), Err(Error::Protocol));
}

#[test]
fn consent_requires_nine_unanswered_probes_and_the_final_response_window() {
    let tick = Instant::now();
    let mut consent = Consent::default();
    consent.observe(tick, 4, 2);
    // An arbitrarily late tick with no new checks is not a failed check.
    consent.observe(tick + Duration::from_secs(60), 4, 2);
    assert!(!consent.failed);
    for count in 1..=9 {
        let now = tick + Duration::from_secs(60 + count * 2);
        consent.observe(now, 4 + count, 2);
        assert!(!consent.failed);
    }
    consent.observe(tick + Duration::from_secs(80), 14, 2);
    assert!(!consent.failed);
    consent.observe(tick + Duration::from_secs(81), 14, 2);
    assert!(consent.failed);
    consent.observe(tick + Duration::from_secs(82), 14, 3);
    assert!(!consent.failed);
    consent.observe(tick + Duration::from_secs(142), 15, 3);
    assert!(!consent.failed);
}

// T3: an elapsed caller tick may start a consent check, but must not revoke
// an established channel before that check has had a chance to get a response.
#[tokio::test(start_paused = true)]
async fn t3_elapsed_ticks_keep_exact_delivery_and_dead_remote_loses_readiness() {
    let (mut a, mut b) = Box::pin(pair()).await;
    for seconds in [12, 59] {
        tokio::time::advance(Duration::from_secs(seconds)).await;
        a.timeout(now()).await.unwrap();
        b.timeout(now()).await.unwrap();
        assert!(a.ready() && b.ready());
        let left = format!("{{\"delegation_id\":\"left-{seconds}\"}}");
        let right = format!("{{\"delegation_id\":\"right-{seconds}\"}}");
        a.send_text(&left).await.unwrap();
        b.send_text(&right).await.unwrap();
        let ceiling = Instant::now() + Duration::from_secs(5);
        while a.text.items_empty() || b.text.items_empty() {
            assert!(Instant::now() < ceiling, "elapsed-tick delivery stalled");
            a.receive(now()).await.unwrap();
            b.receive(now()).await.unwrap();
            tokio::task::yield_now().await;
        }
        assert_eq!(a.pop_text(), Some(right));
        assert_eq!(b.pop_text(), Some(left));
    }
    // Finish pending consent exchanges before removing the remote endpoint.
    let ceiling = Instant::now() + Duration::from_secs(5);
    while a.consent.baseline != a.consent.sent || b.consent.baseline != b.consent.sent {
        assert!(Instant::now() < ceiling, "consent response stalled");
        a.receive(now()).await.unwrap();
        b.receive(now()).await.unwrap();
        tokio::task::yield_now().await;
    }
    b.close();
    for _ in 0..12 {
        tokio::time::advance(Duration::from_secs(2)).await;
        a.timeout(now()).await.unwrap();
    }
    assert!(!a.ready());
    assert_eq!(a.send_text("undeliverable").await, Err(Error::NotReady));
    assert_eq!((a.pop_text(), b.pop_text()), (None, None));
    let socket = a.socket_owner();
    assert!(socket.upgrade().is_some());
    a.close();
    assert!(socket.upgrade().is_none());
}

// T3: a retained real socket must prevent release proof even after peer close.
#[tokio::test]
async fn t3_socket_release_witness_follows_last_actual_owner() {
    let mut peer = peer().await;
    let socket = peer.socket_owner();
    let retained = socket.upgrade().unwrap();
    peer.close();
    assert!(socket.upgrade().is_some());
    drop(retained);
    assert!(socket.upgrade().is_none());
    peer.close();
    assert!(socket.upgrade().is_none());
}
