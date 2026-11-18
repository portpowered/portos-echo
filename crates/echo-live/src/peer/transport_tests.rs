//! T3: real local ICE/DTLS/SRTP/SCTP; synthetic Opus and opaque JSON only.
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use crate::{AudioFrame, Error, LivePeer};

fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

async fn peer() -> Result<LivePeer, Error> {
    Box::pin(LivePeer::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), now())).await
}

async fn step(a: &mut LivePeer, b: &mut LivePeer, deadline: Instant) -> Result<(), Error> {
    if now() >= deadline {
        return Err(Error::Deadline);
    }
    let next = a.flush().await?.min(b.flush().await?).min(deadline);
    let event = {
        let left = a.readable();
        let right = b.readable();
        let timer = tokio::time::sleep_until(next.into());
        // Pin explicitly below so readiness futures are dropped before mutation.
        tokio_readiness(left, right, timer).await?
    };
    match event {
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

async fn tokio_readiness(
    left: impl Future<Output = Result<(), Error>>,
    right: impl Future<Output = Result<(), Error>>,
    timer: impl Future<Output = ()>,
) -> Result<u8, Error> {
    use std::task::Poll;
    let mut left = std::pin::pin!(left);
    let mut right = std::pin::pin!(right);
    let mut timer = std::pin::pin!(timer);
    std::future::poll_fn(|cx| {
        if let Poll::Ready(result) = left.as_mut().poll(cx) {
            return Poll::Ready(result.map(|()| 0));
        }
        if let Poll::Ready(result) = right.as_mut().poll(cx) {
            return Poll::Ready(result.map(|()| 1));
        }
        if timer.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Ok(2));
        }
        Poll::Pending
    })
    .await
}

async fn pair() -> Result<(LivePeer, LivePeer), Error> {
    let mut a = peer().await?;
    let mut b = peer().await?;
    let offer = a.offer().await?;
    assert_eq!(offer.matches("m=audio").count(), 1);
    assert_eq!(offer.matches("a=sendrecv").count(), 1);
    assert!(offer.contains("opus/48000/2"));
    assert!(!offer.contains("m=video"));
    let answer = b.answer(&offer).await?;
    a.apply_answer(&answer).await?;
    let deadline = now() + Duration::from_secs(10);
    while !(a.ready() && b.ready()) {
        step(&mut a, &mut b, deadline).await?;
    }
    Ok((a, b))
}

// The bridge's --sim-clock can stay frozen while the remote peer's protocol
// clock advances. Incoming network work must not require a local timer tick.
#[tokio::test(start_paused = true)]
async fn t3_frozen_caller_negotiates_and_exchanges() -> Result<(), Error> {
    let tick = now();
    let mut a = peer().await?;
    let mut b = peer().await?;
    let offer = a.offer().await?;
    let answer = b.answer(&offer).await?;
    a.apply_answer(&answer).await?;
    let ceiling = tick + Duration::from_secs(5);
    while !(a.ready() && b.ready()) {
        frozen_step(&mut a, &mut b, tick, ceiling).await?;
    }
    a.send_audio(frame(7, 4000), tick).await?;
    b.send_text("{\"id\":\"frozen\"}").await?;
    let (mut audio, mut text) = (None, None);
    while audio.is_none() || text.is_none() {
        frozen_step(&mut a, &mut b, tick, ceiling).await?;
        audio = audio.or_else(|| b.pop_audio());
        text = text.or_else(|| a.pop_text());
    }
    assert_eq!(audio, Some(frame(7, 4000)));
    assert_eq!(text.as_deref(), Some("{\"id\":\"frozen\"}"));
    assert_eq!((a.dropped_audio(), b.dropped_audio()), (0, 0));
    Ok(())
}

async fn frozen_step(a: &mut LivePeer, b: &mut LivePeer, tick: Instant, ceiling: Instant) -> Result<(), Error> {
    assert!(now() < ceiling, "negotiation/data stalled at frozen caller tick");
    if a.flush().await? <= tick {
        a.timeout(tick).await?;
    }
    let next = b.flush().await?.min(ceiling);
    let event = tokio_readiness(a.readable(), b.readable(), tokio::time::sleep_until(next.into())).await?;
    match event {
        0 => {
            a.receive(tick).await?;
        }
        1 => {
            b.receive(now()).await?;
        }
        _ => b.timeout(now()).await?,
    }
    Ok(())
}

// RFC 6716 Opus comfort-noise packet: one 20-ms full-band frame.
fn frame(sequence: u16, marker: u32) -> AudioFrame {
    AudioFrame {
        sequence,
        timestamp: marker + u32::from(sequence) * 960,
        payload: vec![0xf8, 0xff, 0xfe],
    }
}

async fn exchange(a: &mut LivePeer, b: &mut LivePeer, marker: u32) -> Result<(), Error> {
    let deadline = now() + Duration::from_secs(10);
    let mut left = Vec::new();
    let mut right = Vec::new();
    for sequence in 0..100 {
        a.send_audio(frame(sequence, marker), now()).await?;
        b.send_audio(frame(sequence, marker + 100_000), now()).await?;
        while left.len() <= usize::from(sequence) || right.len() <= usize::from(sequence) {
            step(a, b, deadline).await?;
            while let Some(packet) = a.pop_audio() {
                left.push(packet);
            }
            while let Some(packet) = b.pop_audio() {
                right.push(packet);
            }
        }
    }
    assert_eq!(left, (0..100).map(|n| frame(n, marker + 100_000)).collect::<Vec<_>>());
    assert_eq!(right, (0..100).map(|n| frame(n, marker)).collect::<Vec<_>>());
    exchange_text(a, b, marker, deadline).await
}

async fn exchange_text(a: &mut LivePeer, b: &mut LivePeer, marker: u32, deadline: Instant) -> Result<(), Error> {
    let a_text: Vec<_> = (0..3)
        .map(|id| format!("{{\"id\":\"a-{marker}-{id}\",\"text\":\"hello ☃\"}}"))
        .collect();
    let b_text: Vec<_> = (0..3)
        .map(|id| format!("{{\"id\":\"b-{marker}-{id}\",\"text\":\"reply\"}}"))
        .collect();
    for (a_msg, b_msg) in a_text.iter().zip(&b_text) {
        a.send_text(a_msg).await?;
        b.send_text(b_msg).await?;
    }
    let (mut received_a, mut received_b) = (Vec::new(), Vec::new());
    while received_a.len() < 3 || received_b.len() < 3 {
        step(a, b, deadline).await?;
        while let Some(text) = a.pop_text() {
            received_a.push(text);
        }
        while let Some(text) = b.pop_text() {
            received_b.push(text);
        }
    }
    assert_eq!(received_a, b_text);
    assert_eq!(received_b, a_text);
    assert_eq!(
        (a.pop_audio(), b.pop_audio(), a.pop_text(), b.pop_text()),
        (None, None, None, None)
    );
    assert_eq!((a.dropped_audio(), b.dropped_audio()), (0, 0));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn t3_bidirectional_opus_and_events() -> Result<(), Error> {
    let (mut a, mut b) = Box::pin(pair()).await?;
    exchange(&mut a, &mut b, 0).await?;
    writeln!(std::io::stdout(),
        "A_to_B_frames=100 B_to_A_frames=100 frame_ms=20 sequence=0..99 A_to_B_json=3 B_to_A_json=3 bytes_equal=true order_equal=true ice=connected channel=oai-events"
    ).map_err(|_| Error::Io)?;
    a.close();
    b.close();
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn t3_unanswered_offer_deadline_and_cancel() -> Result<(), Error> {
    let mut a = peer().await?;
    let mut b = peer().await?;
    let socket = a.socket_owner();
    assert!(socket.upgrade().is_some());
    a.offer().await?;
    let deadline = now() + Duration::from_secs(1);
    while now() < deadline {
        step(&mut a, &mut b, deadline).await?;
    }
    assert_eq!(step(&mut a, &mut b, deadline).await, Err(Error::Deadline));
    assert!(!a.ready());
    assert_eq!(a.send_text("not delivered").await, Err(Error::NotReady));
    assert_eq!(
        (a.pop_audio(), b.pop_audio(), a.pop_text(), b.pop_text()),
        (None, None, None, None)
    );
    // Dropping the pending readiness future does not consume or spawn anything.
    {
        let pending = a.readable();
        tokio::pin!(pending);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(pending.as_mut().poll(cx).is_pending())).await);
    }
    a.close();
    b.close();
    assert_eq!(a.readable().await, Err(Error::Closed));
    assert!(socket.upgrade().is_none());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn t3_close_preserves_delivery_and_releases_socket() -> Result<(), Error> {
    let (mut a, mut b) = Box::pin(pair()).await?;
    let socket = a.socket_owner();
    assert!(socket.upgrade().is_some());
    let mut invalid = frame(0, 42);
    invalid.payload.clear();
    assert_eq!(a.send_audio(invalid, now()).await, Err(Error::Payload));
    assert_eq!(a.send_text(&"x".repeat(16_385)).await, Err(Error::Full));
    a.send_audio(frame(0, 42), now()).await?;
    a.send_text("{\"id\":\"ack\"}").await?;
    let deadline = now() + Duration::from_secs(5);
    let (mut audio, mut text) = (None, None);
    while audio.is_none() || text.is_none() {
        step(&mut a, &mut b, deadline).await?;
        if audio.is_none() {
            audio = b.pop_audio();
        }
        if text.is_none() {
            text = b.pop_text();
        }
    }
    while a.receive(now()).await? {}
    {
        let pending = a.readable();
        tokio::pin!(pending);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(pending.as_mut().poll(cx).is_pending())).await);
    }
    a.close();
    a.close();
    b.close();
    assert_eq!(audio, Some(frame(0, 42)));
    assert_eq!(text.as_deref(), Some("{\"id\":\"ack\"}"));
    assert_eq!(a.send_audio(frame(1, 42), now()).await, Err(Error::Closed));
    assert_eq!(a.send_text("closed").await, Err(Error::Closed));
    assert_eq!(a.receive(now()).await, Err(Error::Closed));
    assert_eq!(a.readable().await, Err(Error::Closed));
    assert_eq!(a.timeout(now()).await, Err(Error::Closed));
    assert_eq!((b.pop_audio(), b.pop_text()), (None, None));
    assert!(socket.upgrade().is_none());
    Ok(())
}

async fn journey(marker: u32) -> Result<(), Error> {
    let (mut a, mut b) = Box::pin(pair()).await?;
    exchange(&mut a, &mut b, marker).await?;
    a.close();
    b.close();
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn t3_two_pairs_are_isolated() -> Result<(), Error> {
    let left = tokio::spawn(Box::pin(journey(1000)));
    let right = tokio::spawn(Box::pin(journey(2000)));
    left.await.map_err(|_| Error::Protocol)??;
    right.await.map_err(|_| Error::Protocol)??;
    Ok(())
}
