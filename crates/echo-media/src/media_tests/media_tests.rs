use std::io::Write;
// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host transport tests; Modified by Port OS.
use crate::{MediaPeer, Peer, PeerConfig, PeerState};
use std::{
    net::{IpAddr, Ipv4Addr},
    time::{Duration, Instant},
};

mod media_stun;
mod media_tone;
mod media_unit;

fn config() -> PeerConfig {
    PeerConfig {
        interface_allowlist: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        allow_loopback: true,
        ..PeerConfig::default()
    }
}

async fn pair() -> (Peer, Peer) {
    let a = Peer::create(config()).await.unwrap();
    let b = Peer::build(config(), true).await.unwrap();
    let offer = a.offer().await.unwrap();
    assert_eq!(offer.lines().filter(|l| l.starts_with("m=audio ")).count(), 1);
    assert!(offer.contains("opus/48000/2"));
    assert!(!offer.contains("m=video"));
    assert_eq!(offer.lines().filter(|l| l.starts_with("m=application ")).count(), 1);
    b.begin().unwrap();
    b.remote(&offer, false).await.unwrap();
    let answer = b.local(true).await.unwrap();
    let start = Instant::now();
    a.apply_answer(&answer).await.unwrap();
    let mut a_states = a.states();
    let mut b_states = b.states();
    tokio::time::timeout(Duration::from_secs(2), async {
        a_states.wait_for(|s| *s == PeerState::Connected).await.unwrap();
        b_states.wait_for(|s| *s == PeerState::Connected).await.unwrap();
    })
    .await
    .unwrap();
    writeln!(std::io::stdout(), "connect_ms={}", start.elapsed().as_millis()).unwrap();
    assert!(start.elapsed() <= Duration::from_secs(2));
    (a, b)
}

#[tokio::test]
async fn t3_peer_lifecycle_close_connected_and_isolation() {
    let ((mut a, mut b), (mut c, mut d)) = tokio::join!(pair(), pair());
    // Distinct public PCM journeys on simultaneous independent peers.
    let virtual_time = media_tone::VirtualTime::start();
    let clock = media_tone::Clock::new(2);
    tokio::join!(
        clock.drive(),
        media_tone::journey(&a, &mut b, 1000, &clock),
        media_tone::journey(&c, &mut d, 1400, &clock)
    );
    drop(virtual_time);
    for peer in [&mut a, &mut b, &mut c, &mut d] {
        peer.close().await.unwrap();
        assert_eq!(*peer.states().borrow(), PeerState::Closed);
        assert_eq!(peer.send([0; 320]), Err(crate::Error::Closed));
        assert_eq!(peer.recv().await, Ok(None));
        peer.close().await.unwrap();
        assert!(peer.shared.tasks.lock().unwrap().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn t3_peer_lifecycle_unanswered_deadline_and_close_connecting() {
    let mut peer = Peer::create(config()).await.unwrap();
    assert_eq!(peer.send([0; 320]), Err(crate::Error::NotConnected));
    peer.offer().await.unwrap();
    assert_eq!(peer.offer().await, Err(crate::Error::State));
    tokio::time::advance(Duration::from_secs(16)).await;
    let mut state = peer.states();
    state.wait_for(|s| *s == PeerState::Failed).await.unwrap();
    assert_eq!(peer.send([0; 320]), Err(crate::Error::Deadline));
    peer.close().await.unwrap();
    let mut peer = Peer::create(config()).await.unwrap();
    peer.offer().await.unwrap();
    peer.shared.recv.push([42; 320]);
    peer.close().await.unwrap();
    assert_eq!(peer.recv().await, Ok(None));
}

#[tokio::test]
async fn t3_peer_lifecycle_releases_all_advertised_sockets() {
    let mut peer = Peer::create(config()).await.unwrap();
    let offer = peer.offer().await.unwrap();
    let mut sockets = std::collections::BTreeSet::new();
    for line in offer.lines().filter(|l| l.starts_with("a=candidate:")) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields[7] == "host" {
            // ICE-TCP active candidates advertise discard port 9, not a listener.
            if fields[2].eq_ignore_ascii_case("tcp") && fields.windows(2).any(|w| w == ["tcptype", "active"]) {
                assert_eq!(fields[5], "9");
                continue;
            }
            if fields[2].eq_ignore_ascii_case("tcp") {
                assert!(fields.windows(2).any(|w| w == ["tcptype", "passive"]));
            }
            sockets.insert((
                fields[2].to_ascii_lowercase(),
                fields[4].parse::<IpAddr>().unwrap(),
                fields[5].parse::<u16>().unwrap(),
            ));
        }
    }
    assert!(sockets.iter().any(|s| s.0 == "udp"));
    assert!(sockets.iter().any(|s| s.0 == "tcp"));
    for (protocol, ip, port) in &sockets {
        let address = std::net::SocketAddr::new(*ip, *port);
        // Prove these are owned sockets before requiring close to release them.
        if protocol == "udp" {
            assert!(std::net::UdpSocket::bind(address).is_err());
        } else {
            assert!(std::net::TcpListener::bind(address).is_err());
        }
    }
    peer.close().await.unwrap();
    for (protocol, ip, port) in sockets {
        let address = std::net::SocketAddr::new(ip, port);
        if protocol == "udp" {
            let socket = std::net::UdpSocket::bind(address).unwrap();
            assert_eq!(socket.local_addr().unwrap(), address);
        } else {
            let socket = std::net::TcpListener::bind(address).unwrap();
            assert_eq!(socket.local_addr().unwrap(), address);
        }
    }
}
