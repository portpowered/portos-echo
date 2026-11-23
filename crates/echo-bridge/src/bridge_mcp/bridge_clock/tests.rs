use super::{
    Action, Advance, parse,
    receipt::{self, Context},
};
use crate::Error;
use serde_json::json;
use std::{future::Future, task::Poll, time::Duration};

#[tokio::test(start_paused = true)]
async fn terminal_control_allows_cleanup_deadlines_without_another_clock_command() {
    for clean in [true, false] {
        if !clean {
            tokio::time::pause();
        }
        let (stop, cancelled) = tokio::sync::oneshot::channel();
        let collected = std::cell::Cell::new(false);
        let owner = async {
            assert_eq!(cancelled.await, Ok(clean));
            // Model a drain blocked on a client that no longer reads. The
            // cleanup timeout must expire even inside the always-ready driver.
            assert!(
                tokio::time::timeout(Duration::from_millis(1), std::future::pending::<()>())
                    .await
                    .is_err()
            );
            collected.set(true);
            Ok(())
        };
        let control = if clean { Ok(()) } else { Err(Error::Control) };
        assert_eq!(
            receipt::bounded(
                super::drive(super::finish(owner, stop, control)),
                Duration::from_secs(1)
            )
            .await,
            if clean { Ok(()) } else { Err(Error::Control) }
        );
        assert!(collected.get(), "terminal input must await actual owner cleanup");
    }
}

#[test]
fn playout_control_rejects_malformed_and_keeps_legacy_compatible() {
    assert_eq!(
        parse(br#"{"action":"advance_playout","milliseconds":20}"#),
        Ok(Action::Advance(Advance {
            milliseconds: 20,
            observed: true
        }))
    );
    for text in [
        r#"{"action":"advance_playout","milliseconds":-1}"#,
        r#"{"action":"advance_playout","milliseconds":0,"extra":true}"#,
        r#"{"action":"advance_playout","milliseconds":null}"#,
    ] {
        assert_eq!(parse(text.as_bytes()), Err(Error::Control));
    }
    assert!(Context::current().is_none());
}

#[tokio::test(start_paused = true)]
async fn receipt_missing_screen_and_proxy_failure_never_acknowledge_success() {
    let context = Context::new();
    assert_eq!(context.advance(20).await, Err(Error::Control));
    context.failed();
    assert_eq!(context.advance(0).await, Err(Error::Control));
    assert_eq!(
        receipt::bounded(std::future::pending::<Result<(), Error>>(), Duration::ZERO).await,
        Err(Error::Control)
    );
}

#[tokio::test(start_paused = true)]
async fn voice_receipt_waits_for_explicit_acknowledgement_and_closed_owner_fails() {
    let context = Context::new();
    let mut voice = context.voice().unwrap();
    let drain = context.drain();
    tokio::pin!(drain);
    assert!(std::future::poll_fn(|cx| Poll::Ready(drain.as_mut().poll(cx).is_pending())).await);
    voice.acknowledge().await;
    // Receipt progresses past voice only after acknowledgement; screen is absent.
    assert_eq!(drain.await, Err(Error::Control));
    let drain = context.drain();
    tokio::pin!(drain);
    assert!(std::future::poll_fn(|cx| Poll::Ready(drain.as_mut().poll(cx).is_pending())).await);
    drop(voice);
    assert_eq!(drain.await, Err(Error::Control));
}

#[tokio::test(start_paused = true)]
async fn successful_voice_completion_acknowledges_a_pending_drain_before_retiring() {
    let context = Context::new();
    let voice = context.voice().unwrap();
    let mut publisher = context.publisher().unwrap();
    let drain = context.drain();
    tokio::pin!(drain);
    assert!(std::future::poll_fn(|cx| Poll::Ready(drain.as_mut().poll(cx).is_pending())).await);
    voice.finish();
    // The step must reach the publisher request, rather than fail on a lost
    // voice receipt. A missing screen is checked only after that acknowledgement.
    assert!(std::future::poll_fn(|cx| Poll::Ready(drain.as_mut().poll(cx).is_pending())).await);
    publisher.acknowledge().await;
    assert_eq!(drain.await, Err(Error::Control));
    assert!(context.voice().is_ok());
}

#[tokio::test(start_paused = true)]
async fn unmatched_completed_pcm_write_fails_the_step() {
    let context = Context::new();
    context.wrote(&65_i16.to_le_bytes().repeat(480)).unwrap();
    assert_eq!(context.advance(0).await, Err(Error::Control));
}

#[tokio::test(start_paused = true)]
async fn missing_task_or_pcm_acknowledgement_expires_without_advancing_time() {
    let context = Context::new();
    let voice_owner = context.voice().unwrap();
    let before = tokio::time::Instant::now();
    assert_eq!(
        receipt::bounded(context.advance(20), Duration::from_millis(1)).await,
        Err(Error::Control)
    );
    assert_eq!(tokio::time::Instant::now(), before);
    drop(voice_owner);
    let publisher_owner = context.publisher().unwrap();
    assert_eq!(
        receipt::bounded(context.advance(20), Duration::from_millis(1)).await,
        Err(Error::Control)
    );
    drop(publisher_owner);
    let context = Context::new();
    context.submitted("reply").unwrap();
    assert_eq!(
        receipt::bounded(context.advance(20), Duration::from_millis(1)).await,
        Err(Error::Control)
    );
    assert_eq!(tokio::time::Instant::now(), before);
}

/// T3: real local publication pump, no accounts or hardware.
#[tokio::test(start_paused = true)]
async fn t3_receipt_contains_one_epoch_exact_samples_and_confirmed_projected_states() {
    use echo_contracts::secret::Secret;
    use echo_screen_contract::UiState;
    use std::sync::Arc;
    let context = Context::new();
    let (states, input) = tokio::sync::mpsc::channel(4);
    let server = Arc::new(
        echo_screen_server::serve(
            ([127, 0, 0, 1], 0).into(),
            Secret::new("synthetic-clock-pair".into()),
            input,
        )
        .await
        .unwrap(),
    );
    context.screen(&server).unwrap();
    let mut voice = context.voice().unwrap();
    let mut publisher = context.publisher().unwrap();
    context.submitted("earcon").unwrap();
    context.wrote(&65_i16.to_le_bytes().repeat(480)).unwrap();
    context.submitted("earcon").unwrap();
    context.discarded("earcon", 1).unwrap();
    states.send(UiState::Speaking).await.unwrap();
    context.projected(&UiState::Speaking).unwrap();
    let step = async {
        let receipt = context.advance(20).await.unwrap();
        assert_eq!(
            receipt,
            json!({
                "event":"bridge.clock_advanced", "milliseconds":20, "epoch_ms":20,
                "pcm":[{"epoch_ms":0,"pcm_bytes":960,"nonzero_samples":480,"audible_samples":480,"source":"earcon"},
                    {"epoch_ms":0,"pcm_bytes":960,"nonzero_samples":480,"audible_samples":480,"source":"reply"}],
                "states":[{"epoch_ms":0,"state":{"state":"speaking"}}],
            })
        );
    };
    let owner = async {
        voice.acknowledge().await;
        publisher.acknowledge().await;
        voice.acknowledge().await;
        // Work can become ready while the first publication flush is awaited.
        // The final voice fence must detect it and flush another complete pass.
        context.submitted("reply").unwrap();
        context.wrote(&65_i16.to_le_bytes().repeat(480)).unwrap();
        voice.acknowledge().await;
        publisher.acknowledge().await;
        voice.acknowledge().await;
        voice.acknowledge().await;
        publisher.acknowledge().await;
        voice.acknowledge().await;
    };
    super::drive(async {
        tokio::join!(step, owner);
    })
    .await;
    drop(voice);
    drop(publisher);
    assert_eq!(context.advance(0).await, Err(Error::Control));
    Arc::try_unwrap(server).ok().unwrap().shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn receipt_rejects_unknown_sources_and_cut_cannot_hide_reply_or_written_pcm() {
    let context = Context::new();
    assert_eq!(context.submitted("unknown"), Err(Error::Control));
    context.submitted("reply").unwrap();
    assert_eq!(context.discarded("earcon", 1), Err(Error::Control));
    let context = Context::new();
    context.submitted("earcon").unwrap();
    context.wrote(&65_i16.to_le_bytes()).unwrap();
    assert_eq!(context.discarded("earcon", 1), Err(Error::Control));
    assert_eq!(context.advance(0).await, Err(Error::Control));
    let context = Context::new();
    context.submitted("reply").unwrap();
    context.submitted("earcon").unwrap();
    assert_eq!(context.discarded("reply", 1), Err(Error::Control));
    assert_eq!(context.discarded("reply", 2), Err(Error::Control));
}

/// T3: real loopback sockets at the guarded bridge write boundary.
#[tokio::test(start_paused = true)]
async fn t3_proxy_stamps_only_complete_forwarded_pcm_and_preserves_bytes() {
    use echo_contracts::wyoming::{Event, Header, encode};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    let host = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let context = Context::new();
    let proxy = super::proxy::Proxy::bind(host.local_addr().unwrap(), context.clone())
        .await
        .unwrap();
    let event = Event {
        header: Header {
            r#type: "audio-chunk".into(),
            data: Some(json!({"rate":24000,"width":2,"channels":1})),
            data_length: 0,
            payload_length: 960,
        },
        data: None,
        payload: 65_i16.to_le_bytes().repeat(480),
    };
    let wire = encode(&event);
    let mut peer = TcpStream::connect(proxy.address).await.unwrap();
    let (mut sink, _) = host.accept().await.unwrap();
    peer.write_all(&wire).await.unwrap();
    let mut received = vec![0; wire.len()];
    sink.read_exact(&mut received).await.unwrap();
    assert_eq!(received, wire);
    // No submitted frame was registered, so a completed proxy write fails closed.
    assert_eq!(super::drive(context.advance(0)).await, Err(Error::Control));
}
