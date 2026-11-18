// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Modified by Port OS: component policy, overflow, and validation assertions.
use crate::{Error, MediaPeer, Peer, PeerConfig, PeerState, media_queue::Queue, media_signal};

#[tokio::test]
async fn setup_observer_is_once_per_peer_and_terminal_after_close() {
    use crate::SetupStage::{DataChannelOpen, IceConnected};
    let mut a = Peer::controlled();
    let mut b = Peer::controlled();
    let first = super::setup_observations(&a);
    let second = super::setup_observations(&b);
    assert_eq!(a.observe_setup(std::sync::Arc::new(|_| Ok(()))), Err(Error::State));
    a.shared.begin().unwrap();
    b.shared.begin().unwrap();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let shared = &a.shared;
            scope.spawn(move || shared.setup_stage(IceConnected));
        }
    });
    assert_eq!(*first.lock().unwrap(), [IceConnected]);
    assert_eq!(*a.states().borrow(), PeerState::Connecting);
    assert_eq!(*second.lock().unwrap(), []);
    a.shared.setup_stage(DataChannelOpen);
    a.shared.connected(false);
    assert_eq!(*a.states().borrow(), PeerState::Connecting);
    a.shared.connected(true);
    assert_eq!(*a.states().borrow(), PeerState::Connected);
    a.shared.setup_stage(DataChannelOpen);
    assert_eq!(*first.lock().unwrap(), [IceConnected, DataChannelOpen]);
    b.shared.setup_stage(IceConnected);
    assert_eq!(*second.lock().unwrap(), [IceConnected]);
    a.close().await.unwrap();
    b.close().await.unwrap();
    for peer in [&a, &b] {
        peer.shared.setup_stage(DataChannelOpen);
        peer.shared.setup_stage(IceConnected);
        peer.shared.connected(true);
        assert_eq!(*peer.states().borrow(), PeerState::Closed);
    }
    assert_eq!(*first.lock().unwrap(), [IceConnected, DataChannelOpen]);
    assert_eq!(*second.lock().unwrap(), [IceConnected]);
}

#[tokio::test]
async fn setup_observer_rejects_late_installation_and_failure_prevents_connected() {
    use crate::SetupStage::{DataChannelOpen, IceConnected};
    let mut peer = Peer::controlled();
    peer.shared.begin().unwrap();
    assert_eq!(peer.observe_setup(std::sync::Arc::new(|_| Ok(()))), Err(Error::State));
    peer.close().await.unwrap();
    assert_eq!(peer.observe_setup(std::sync::Arc::new(|_| Ok(()))), Err(Error::State));
    let mut peer = Peer::controlled();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    peer.observe_setup(std::sync::Arc::new(move |_| {
        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(Error::State)
    }))
    .unwrap();
    peer.shared.begin().unwrap();
    peer.shared.setup_stage(IceConnected);
    peer.shared.setup_stage(DataChannelOpen);
    peer.shared.connected(true);
    peer.shared.connected(false);
    assert_eq!(*peer.states().borrow(), PeerState::Failed);
    assert_eq!(peer.send([0; 320]), Err(Error::Transport));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    peer.close().await.unwrap();
}

#[tokio::test]
async fn setup_ice_callbacks_deduplicate_connected_and_completed_without_readiness() {
    use webrtc::peer_connection::{PeerConnectionEventHandler, RTCIceConnectionState};
    let mut peer = Peer::controlled();
    let stages = super::setup_observations(&peer);
    let events = media_signal::Events {
        shared: peer.shared.clone(),
        gathered: std::sync::Arc::new(tokio::sync::Notify::new()),
        answerer: false,
    };
    peer.shared.begin().unwrap();
    for state in [
        RTCIceConnectionState::Checking,
        RTCIceConnectionState::Connected,
        RTCIceConnectionState::Completed,
        RTCIceConnectionState::Connected,
    ] {
        events.on_ice_connection_state_change(state).await;
        assert_eq!(*peer.states().borrow(), PeerState::Connecting);
    }
    assert_eq!(*stages.lock().unwrap(), [crate::SetupStage::IceConnected]);
    peer.close().await.unwrap();
    events
        .on_ice_connection_state_change(RTCIceConnectionState::Connected)
        .await;
    assert_eq!(*stages.lock().unwrap(), [crate::SetupStage::IceConnected]);
}

#[test]
fn setup_close_waits_for_inflight_observer_and_discards_late_notifications() {
    use crate::{SetupStage::IceConnected, media_state::Shared};
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    let shared = Shared::new(1);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let count = Arc::new(AtomicUsize::new(0));
    let (callback_entered, callback_release, calls) = (entered.clone(), release.clone(), count.clone());
    shared
        .observe_setup(Arc::new(move |_| {
            callback_entered.wait();
            callback_release.wait();
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }))
        .unwrap();
    shared.begin().unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| shared.setup_stage(IceConnected));
        entered.wait();
        let close = scope.spawn(|| {
            shared.close();
            assert_eq!(count.load(Ordering::SeqCst), 1);
        });
        release.wait();
        close.join().unwrap();
    });
    shared.setup_stage(crate::SetupStage::DataChannelOpen);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(*shared.state.borrow(), PeerState::Closed);
}

#[tokio::test]
async fn queue_oldest_order_exact_count_and_close() {
    let queue = Queue::new(3);
    for marker in 0..3 {
        queue.push(marker);
    }
    assert_eq!(queue.drops(), 0);
    queue.push(3);
    queue.push(4);
    assert_eq!(queue.drops(), 2);
    for marker in 2..5 {
        assert_eq!(queue.pop().await, Some(marker));
    }
    let waiting = queue.pop();
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(std::time::Duration::ZERO, &mut waiting)
            .await
            .is_err()
    );
    queue.close();
    assert_eq!(waiting.await, None);
    queue.push(8);
    assert_eq!(queue.pop().await, None);
    assert_eq!(queue.drops(), 2);
}

#[tokio::test]
async fn queue_cancelled_wait_and_barrier_producer() {
    let queue = Queue::new(3);
    let pending = queue.pop();
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(std::time::Duration::ZERO, &mut pending)
            .await
            .is_err()
    );
    let barrier = tokio::sync::Barrier::new(2);
    let produce = async {
        for marker in 0..5 {
            queue.push(marker);
        }
        barrier.wait().await;
    };
    let consume = async {
        barrier.wait().await;
        for marker in 2..5 {
            assert_eq!(queue.pop().await, Some(marker));
        }
    };
    tokio::join!(produce, consume);
    assert_eq!(queue.drops(), 2);
}

#[tokio::test]
async fn peer_queue_public_boundary_oldest_order_exact_counts() {
    // Controlled transport state and no workers: overflow is deterministic.
    let mut peer = Peer::controlled();
    peer.shared.state.send_replace(PeerState::Connected);
    for marker in 0..3 {
        peer.send([marker; 320]).unwrap();
    }
    assert_eq!(peer.dropped_frames().send, 0);
    for marker in 3..5 {
        peer.send([marker; 320]).unwrap();
    }
    assert_eq!(peer.dropped_frames().send, 2);
    for marker in 2..5 {
        assert_eq!(peer.shared.send.pop().await, Some([marker; 320]));
    }
    for marker in 0..5 {
        peer.shared.recv.push([marker; 320]);
    }
    assert_eq!(peer.dropped_frames().recv, 2);
    for marker in 2..5 {
        assert_eq!(peer.recv().await, Ok(Some([marker; 320])));
    }
    peer.close().await.unwrap();
}

#[test]
fn failure_config_and_sdp_are_redacted() {
    let mut config = PeerConfig::default();
    config.interface_allowlist.push("127.0.0.1".parse().unwrap());
    assert_eq!(media_signal::validate(&config), Err(Error::InvalidConfig));
    config.allow_loopback = true;
    assert_eq!(media_signal::validate(&config), Ok(()));
    for sdp in ["", "malformed", &"x".repeat(65537)] {
        assert_eq!(media_signal::remote(sdp, true).err(), Some(Error::InvalidSdp));
    }
    assert_eq!(format!("{:?}", Error::InvalidSdp), "InvalidSdp");
}

#[tokio::test]
async fn failure_public_state_and_cancelled_recv() {
    let mut peer = Peer::controlled();
    assert_eq!(peer.apply_answer("malformed").await, Err(Error::State));
    assert_eq!(peer.send([0; 320]), Err(Error::NotConnected));
    assert!(
        tokio::time::timeout(std::time::Duration::ZERO, peer.recv())
            .await
            .is_err()
    );
    peer.shared.recv.push([42; 320]);
    assert_eq!(peer.recv().await, Ok(Some([42; 320])));
    // An already pending public receive is woken by terminal cancellation.
    let shared = peer.shared.clone();
    let receive = peer.recv();
    tokio::pin!(receive);
    assert!(
        tokio::time::timeout(std::time::Duration::ZERO, &mut receive)
            .await
            .is_err()
    );
    shared.state.send_replace(PeerState::Closed);
    shared.clear();
    assert_eq!(receive.await, Ok(None));
}

fn controlled_offer(config: &PeerConfig) -> String {
    let (media, _) = crate::media_track::AudioTrack::new().unwrap();
    let mut core = rtc::peer_connection::RTCPeerConnectionBuilder::new()
        .with_media_engine(media)
        .build(std::time::Instant::now())
        .unwrap();
    core.add_transceiver_from_kind(rtc::rtp_transceiver::rtp_sender::RtpCodecKind::Audio, None)
        .unwrap();
    core.create_data_channel("oai-events", None).unwrap();
    // Documentation-only LAN inventory, passed through production admission and
    // the actual Sans-I/O SDP emitter. No socket or host inventory is read.
    let inventory = ["127.0.0.1", "192.0.2.10", "2001:db8::10", "0.0.0.0", "fe80::1"]; // lint-rules: allow-literal inert socket-free SDP policy assertion, never dialed
    for addr in media_signal::addresses(config, inventory.into_iter().map(|s| s.parse().unwrap())) {
        core.add_local_candidate(webrtc::peer_connection::RTCIceCandidateInit {
            candidate: format!("candidate:1 1 udp 2130706431 {} 5000 typ host", addr.ip()),
            ..Default::default()
        })
        .unwrap();
    }
    let offer = core.create_offer(None).unwrap();
    core.set_local_description(std::time::Instant::now(), offer).unwrap();
    core.local_description().unwrap().sdp
}

#[test]
fn candidate_policy_default_sdp_and_allowlist() {
    let config = PeerConfig::default();
    assert_eq!(config.stun_servers.len(), 0);
    let sdp = controlled_offer(&config);
    assert!(sdp.contains("192.0.2.10 5000 typ host")); // lint-rules: allow-literal inert socket-free SDP policy assertion, never dialed
    assert!(sdp.contains("2001:db8::10 5000 typ host")); // lint-rules: allow-literal inert socket-free SDP policy assertion, never dialed
    assert!(!sdp.contains("127.0.0.1 5000 typ host"));
    assert!(!sdp.contains("fe80::1 5000 typ host")); // lint-rules: allow-literal inert socket-free SDP policy assertion, never dialed
    let sdp = controlled_offer(&PeerConfig {
        interface_allowlist: vec!["192.0.2.10".parse().unwrap()], // lint-rules: allow-literal inert socket-free SDP policy assertion, never dialed
        ..config
    });
    assert!(sdp.contains("192.0.2.10 5000 typ host")); // lint-rules: allow-literal inert socket-free SDP policy assertion, never dialed
    assert!(!sdp.contains("2001:db8::10 5000 typ host")); // lint-rules: allow-literal inert socket-free SDP policy assertion, never dialed
    let sdp = controlled_offer(&super::config());
    assert!(sdp.contains("127.0.0.1 5000 typ host"));
    assert!(!sdp.contains("192.0.2.10 5000 typ host")); // lint-rules: allow-literal inert socket-free SDP policy assertion, never dialed
}

#[test]
fn failure_remote_candidate_occurrence_bound_and_wire_shape() {
    let sdp = controlled_offer(&PeerConfig::default());
    assert_eq!(sdp.lines().filter(|l| l.starts_with("m=audio ")).count(), 1);
    assert!(sdp.contains("opus/48000/2"));
    assert!(!sdp.contains("m=video"));
    assert_eq!(sdp.lines().filter(|l| l.starts_with("m=application ")).count(), 1);
    let without: String =
        sdp.lines()
            .filter(|l| !l.starts_with("a=candidate:"))
            .fold(String::new(), |mut text, line| {
                text.push_str(line);
                text.push_str("\r\n");
                text
            });
    let attr = "a=candidate:1 1 udp 2130706431 127.0.0.1 5000 typ host\r\n"; // lint-rules: allow-literal inert SDP candidate admission fixture, never dialed
    assert_eq!(
        media_signal::remote(&(without.clone() + &attr.repeat(32)), false)
            .unwrap()
            .1
            .len(),
        1
    );
    assert_eq!(
        media_signal::remote(&(without + &attr.repeat(33)), false).err(),
        Some(Error::InvalidSdp)
    );
}

#[test]
fn failure_codec_corruption_duplicate_and_out_of_order() {
    let mut encoder = crate::media_audio::Encoder::new().unwrap();
    let mut decoder = crate::media_audio::Decoder::new().unwrap();
    let mut packet = rtc::rtp::Packet::default();
    packet.header.payload_type = 111;
    while packet.payload.is_empty() {
        if let Some(data) = encoder.encode(&[1000; 320]).unwrap() {
            packet.payload = data.into();
        }
    }
    packet.header.sequence_number = 5;
    decoder.decode(&packet).unwrap();
    assert_eq!(decoder.decode(&packet), Ok(None));
    packet.header.sequence_number = 4;
    assert_eq!(decoder.decode(&packet), Ok(None));
    packet.header.sequence_number = 6;
    packet.payload = vec![0xff; 1276].into();
    assert_eq!(decoder.decode(&packet), Err(Error::Transport));
    packet.payload = vec![0xff].into();
    assert_eq!(decoder.decode(&packet), Err(Error::Audio));
}
