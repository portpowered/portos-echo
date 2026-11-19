use super::*;
use crate::{WyomingClient, wyoming_test_support as peer};
use serde_json::json;
use std::sync::Arc;
use tokio::{
    io::{DuplexStream, duplex},
    sync::{Mutex, oneshot},
    time::Instant,
};

fn pair() -> (WyomingClient, DuplexStream, mpsc::Sender<std::io::Result<DuplexStream>>) {
    let (io, peer) = duplex(4096);
    let (tx, rx) = mpsc::channel(1);
    let rx = Arc::new(Mutex::new(rx));
    let client = WyomingClient::reconnecting(io, ClientOptions::default(), move || {
        let rx = rx.clone();
        async move { rx.lock().await.recv().await.unwrap() }
    });
    (client, peer, tx)
}

#[tokio::test(start_paused = true)]
async fn refused_attempts_follow_capped_backoff_and_timeout() {
    let opts = ClientOptions::default();
    let start = Instant::now();
    let mut attempts = vec![];
    let mut connector = || {
        attempts.push(Instant::now() - start);
        let count = attempts.len();
        async move {
            if count < 9 {
                Err(std::io::Error::other("refused"))
            } else {
                Ok(7)
            }
        }
    };
    assert_eq!(connect(&opts, &mut connector, false).await, 7);
    let expected = [0, 100, 300, 700, 1500, 3100, 5100, 7100, 9100].map(Duration::from_millis);
    assert_eq!(attempts, expected);
    let mut count = 0;
    let mut stalled = || {
        count += 1;
        let attempt = count;
        async move {
            if attempt == 1 {
                std::future::pending().await
            } else {
                Ok(8)
            }
        }
    };
    let start = Instant::now();
    assert_eq!(connect(&opts, &mut stalled, false).await, 8);
    assert_eq!(Instant::now() - start, opts.connect_timeout + opts.reconnect_initial);
}

#[tokio::test(start_paused = true)]
async fn eof_partial_and_malformed_generations_reconnect_without_replay() {
    use tokio::io::AsyncWriteExt;
    for failure in [0, 1, 2] {
        let (mut client, mut server, connections) = pair();
        let mut events = client.events().unwrap();
        let caller = async {
            client.run_satellite().await.unwrap();
            let mut run = events.recv().await.unwrap().unwrap().run.unwrap();
            assert_eq!(run.stats().received, 0);
            let (_source, frames) = mpsc::channel(1);
            let format: echo_contracts::wyoming::AudioFormat =
                serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
            let result = client.play(format, frames).await;
            let error = if failure == 0 {
                Error::Disconnected
            } else {
                Error::Protocol
            };
            assert_eq!(result, Err(error));
            assert_eq!(run.recv().await, Err(error));
            let event = events.recv().await.unwrap().unwrap();
            assert_eq!(event.event, echo_contracts::wyoming::WyomingEvent::Ping(json!("fresh")));
            client.run_satellite().await.unwrap();
            client.close().await.unwrap();
        };
        let server = async {
            assert_eq!(peer::receive(&mut server).await.header.r#type, "run-satellite");
            peer::send(
                &mut server,
                "run-pipeline",
                json!({"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}),
                vec![],
            )
            .await;
            assert_eq!(peer::receive(&mut server).await.header.r#type, "audio-start");
            if failure == 1 {
                server.write_all(b"{\"type\":").await.unwrap();
            }
            if failure == 2 {
                peer::send(&mut server, "audio-chunk", json!({}), vec![9]).await;
            }
            drop(server);
            let (io, mut next) = duplex(4096);
            connections.send(Ok(io)).await.unwrap();
            assert_eq!(peer::receive(&mut next).await.header.r#type, "run-satellite");
            peer::send(&mut next, "ping", json!("fresh"), vec![]).await;
            assert_eq!(tokio::io::AsyncReadExt::read(&mut next, &mut [0]).await.unwrap(), 0);
        };
        tokio::join!(caller, server);
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_invalidates_then_recovers_with_fresh_describe() {
    for mode in 0..3 {
        let play = mode != 0;
        let (mut client, mut server, connections) = pair();
        let mut events = client.events().unwrap();
        let caller = async {
            let error = if play {
                let (tx, rx) = mpsc::channel(1);
                drop(tx);
                let format: echo_contracts::wyoming::AudioFormat =
                    serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
                if mode == 2 {
                    let start = Instant::now();
                    let result = client.play_with_timeout(format, rx, Duration::from_secs(60)).await;
                    assert_eq!(Instant::now() - start, Duration::from_secs(60));
                    result.map(|_| ())
                } else {
                    client.play(format, rx).await.map(|_| ())
                }
            } else {
                client.describe().await.map(|_| ())
            };
            assert_eq!(error, Err(Error::Timeout));
            let (io, mut next) = duplex(4096);
            connections.send(Ok(io)).await.unwrap();
            let (result, ()) = tokio::join!(
                async {
                    assert!(matches!(
                        events.recv().await.unwrap().unwrap().event,
                        echo_contracts::wyoming::WyomingEvent::Ping(_)
                    ));
                    client.describe().await
                },
                async {
                    peer::send(&mut next, "ping", json!("ready"), vec![]).await;
                    assert_eq!(peer::receive(&mut next).await.header.r#type, "describe");
                    peer::send(&mut next, "info", json!({"fresh":true}), vec![]).await;
                }
            );
            result.unwrap();
            client.close().await.unwrap();
        };
        let server = async {
            assert_eq!(
                peer::receive(&mut server).await.header.r#type,
                if play { "audio-start" } else { "describe" }
            );
            if play {
                assert_eq!(peer::receive(&mut server).await.header.r#type, "audio-stop");
            }
            assert_eq!(tokio::io::AsyncReadExt::read(&mut server, &mut [0]).await.unwrap(), 0);
        };
        tokio::join!(caller, server);
    }
}

#[test]
fn disconnected_commands_fail_without_retaining_audio() {
    let (reply, rx) = oneshot::channel();
    assert!(!reject(Some(Command::Describe(reply))));
    assert_eq!(rx.blocking_recv().unwrap(), Err(Error::Disconnected));
    for kind in 0..5 {
        let (reply, rx) = oneshot::channel();
        let command = match kind {
            0 => Command::Register(serde_json::Value::Null, reply),
            1 => Command::Cut(reply),
            2 => Command::Pause(reply),
            3 => Command::EndRun(reply),
            _ => Command::Error("echo-unavailable".into(), "Live unavailable.".into(), reply),
        };
        assert!(!reject(Some(command)));
        assert_eq!(rx.blocking_recv().unwrap(), Err(Error::Disconnected));
    }
    let (reply, rx) = oneshot::channel();
    let (tx, frames) = mpsc::channel(1);
    let format: echo_contracts::wyoming::AudioFormat =
        serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
    assert!(!reject(Some(Command::Play(format, frames, reply))));
    assert_eq!(rx.blocking_recv().unwrap(), Err(Error::Disconnected));
    assert!(tx.is_closed());
    assert!(reject(None));
    let (reply, rx) = oneshot::channel();
    assert!(reject(Some(Command::Close(reply))));
    assert_eq!(rx.blocking_recv().unwrap(), Ok(()));
}

#[tokio::test(start_paused = true)]
async fn repeated_outages_reset_backoff_and_paused_capture_is_not_registered() {
    let (mut client, mut server, connections) = pair();
    let mut events = client.events().unwrap();
    for paused in [false, true, false] {
        let caller = async {
            client.run_satellite().await.unwrap();
            if paused {
                client.pause().await.unwrap();
            }
        };
        let reader = async {
            assert_eq!(peer::receive(&mut server).await.header.r#type, "run-satellite");
            if paused {
                assert_eq!(peer::receive(&mut server).await.header.r#type, "hassmic-end");
                assert_eq!(peer::receive(&mut server).await.header.r#type, "pause-satellite");
            }
        };
        tokio::join!(caller, reader);
        drop(server);
        let (io, mut next) = duplex(4096);
        connections.send(Ok(io)).await.unwrap();
        let start = Instant::now();
        let reader = async {
            if !paused {
                assert_eq!(peer::receive(&mut next).await.header.r#type, "run-satellite");
            }
            peer::send(&mut next, "ping", json!("ready"), vec![]).await;
            assert_eq!(peer::receive(&mut next).await.header.r#type, "describe");
            assert_eq!(Instant::now() - start, Duration::from_millis(100));
            peer::send(&mut next, "info", json!({"ready":true}), vec![]).await;
        };
        let caller = async {
            loop {
                if matches!(
                    events.recv().await.unwrap().unwrap().event,
                    echo_contracts::wyoming::WyomingEvent::Ping(_)
                ) {
                    break;
                }
            }
            assert_eq!(
                client.describe().await.unwrap(),
                echo_contracts::wyoming::WyomingEvent::Info(json!({"ready":true}))
            );
        };
        tokio::join!(caller, reader);
        server = paused_next_generation(&client, &mut events, &connections, next, paused).await;
    }
    client.close().await.unwrap();
    assert_eq!(tokio::io::AsyncReadExt::read(&mut server, &mut [0]).await.unwrap(), 0);
}

#[tokio::test(start_paused = true)]
async fn close_and_drop_cancel_pending_reconnect_without_owned_tasks() {
    for explicit in [true, false] {
        let (client, server, connections) = pair();
        drop(server);
        // The connector only starts after the failed generation is torn down.
        let permit = connections.reserve().await.unwrap();
        drop(permit);
        if explicit {
            client.close().await.unwrap();
        }
        drop(client);
        connections.closed().await;
    }
}

async fn paused_next_generation(
    client: &WyomingClient,
    events: &mut crate::EventStream,
    connections: &mpsc::Sender<std::io::Result<DuplexStream>>,
    next: DuplexStream,
    paused: bool,
) -> DuplexStream {
    // End this generation to keep the next cycle's explicit registration observable.
    drop(next);
    let (io, mut next) = duplex(4096);
    connections.send(Ok(io)).await.unwrap();
    // If capture was enabled it is registered automatically on this socket.
    let caller = async {
        loop {
            if matches!(
                events.recv().await.unwrap().unwrap().event,
                echo_contracts::wyoming::WyomingEvent::Ping(_)
            ) {
                break;
            }
        }
    };
    let reader = async {
        if !paused {
            assert_eq!(peer::receive(&mut next).await.header.r#type, "run-satellite");
        }
        peer::send(&mut next, "ping", json!("cycle"), vec![]).await;
    };
    tokio::join!(caller, reader);
    if !paused {
        let (result, ()) = tokio::join!(client.pause(), async {
            assert_eq!(peer::receive(&mut next).await.header.r#type, "hassmic-end");
            assert_eq!(peer::receive(&mut next).await.header.r#type, "pause-satellite");
        });
        result.unwrap();
    }
    next
}

#[tokio::test(start_paused = true)]
async fn playback_override_survives_twelve_seconds_without_extending_describe_timeout() {
    let (client, mut server, _connections) = pair();
    let caller = async {
        let format: echo_contracts::wyoming::AudioFormat =
            serde_json::from_value(json!({"rate":24000,"width":2,"channels":1})).unwrap();
        let (_send, frames) = mpsc::channel(1);
        assert_eq!(
            client.play_with_timeout(format.clone(), frames, Duration::ZERO).await,
            Err(Error::InvalidOptions)
        );
        let (send, frames) = mpsc::channel(1);
        drop(send);
        assert_eq!(
            client.play_with_timeout(format, frames, Duration::from_secs(60)).await,
            Ok(crate::PlayOutcome::Played)
        );
        let start = Instant::now();
        assert_eq!(client.describe().await, Err(Error::Timeout));
        assert_eq!(Instant::now() - start, Duration::from_secs(5));
    };
    let server = async {
        assert_eq!(peer::receive(&mut server).await.header.r#type, "audio-start");
        assert_eq!(peer::receive(&mut server).await.header.r#type, "audio-stop");
        tokio::time::sleep(Duration::from_secs(12)).await;
        peer::send(&mut server, "played", json!({}), vec![]).await;
        assert_eq!(peer::receive(&mut server).await.header.r#type, "describe");
        assert_eq!(tokio::io::AsyncReadExt::read(&mut server, &mut [0]).await.unwrap(), 0);
    };
    tokio::join!(caller, server);
}

#[tokio::test(start_paused = true)]
async fn configured_watchdog_survives_reconnect_without_duplicate_registration() {
    let (mut client, mut server, connections) = pair();
    let mut events = client.events().unwrap();
    let caller = async {
        for seconds in [0, 2_147_483_648, u32::MAX] {
            assert_eq!(
                client.run_satellite_with_failsafe(seconds).await,
                Err(Error::InvalidOptions)
            );
        }
        client.run_satellite_with_failsafe(605).await.unwrap();
        client.run_satellite_with_failsafe(605).await.unwrap();
        assert!(matches!(
            client.describe().await.unwrap(),
            echo_contracts::wyoming::WyomingEvent::Info(_)
        ));
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if event.event == echo_contracts::wyoming::WyomingEvent::Ping(json!("fresh")) {
                break;
            }
        }
        client.run_satellite_with_failsafe(605).await.unwrap();
        client.close().await.unwrap();
    };
    let device = async {
        let registration = peer::receive(&mut server).await;
        assert_eq!(registration.header.r#type, "run-satellite");
        assert_eq!(registration.header.data, Some(json!({"hassmic_failsafe_seconds":605})));
        assert_eq!(peer::receive(&mut server).await.header.r#type, "describe");
        // Let describe settle before replacing the generation.
        peer::send(&mut server, "info", json!({}), vec![]).await;
        drop(server);
        let (io, mut fresh) = duplex(4096);
        connections.send(Ok(io)).await.unwrap();
        let registration = peer::receive(&mut fresh).await;
        assert_eq!(registration.header.r#type, "run-satellite");
        assert_eq!(registration.header.data, Some(json!({"hassmic_failsafe_seconds":605})));
        peer::send(&mut fresh, "ping", json!("fresh"), vec![]).await;
        assert_eq!(tokio::io::AsyncReadExt::read(&mut fresh, &mut [0]).await.unwrap(), 0);
    };
    tokio::join!(caller, device);
}
