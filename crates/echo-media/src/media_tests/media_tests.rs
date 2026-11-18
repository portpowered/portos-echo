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
    let observations = [setup_observations(&a), setup_observations(&b)];
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
    for stages in observations {
        assert_eq!(
            *stages.lock().unwrap(),
            [crate::SetupStage::IceConnected, crate::SetupStage::DataChannelOpen]
        );
    }
    writeln!(std::io::stdout(), "connect_ms={}", start.elapsed().as_millis()).unwrap();
    assert!(start.elapsed() <= Duration::from_secs(2));
    (a, b)
}

fn setup_observations(peer: &Peer) -> std::sync::Arc<std::sync::Mutex<Vec<crate::SetupStage>>> {
    let stages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = stages.clone();
    peer.observe_setup(std::sync::Arc::new(move |stage| {
        observed.lock().unwrap().push(stage);
        Ok(())
    }))
    .unwrap();
    stages
}

#[tokio::test]
async fn t3_explicit_loopback_create_offer_close_without_multicast() {
    // Also run in a lo-only network namespace: no multicast interface or route.
    let mut peer = Peer::create(config()).await.unwrap();
    assert_eq!(*peer.states().borrow(), PeerState::New);
    let offer = peer.offer().await.unwrap();
    assert_eq!(offer.lines().filter(|l| l.starts_with("m=audio ")).count(), 1);
    assert!(offer.contains("opus/48000/2"));
    assert_eq!(offer.lines().filter(|l| l.starts_with("m=application ")).count(), 1);
    let mut protocols = std::collections::BTreeSet::new();
    for line in offer.lines().filter(|l| l.starts_with("a=candidate:")) {
        let fields: Vec<_> = line.split_whitespace().collect();
        assert_eq!(fields[4].parse::<IpAddr>().unwrap(), Ipv4Addr::LOCALHOST);
        assert_eq!(fields[7], "host");
        protocols.insert(fields[2].to_ascii_lowercase());
    }
    assert_eq!(protocols, ["tcp".to_owned(), "udp".to_owned()].into());
    peer.close().await.unwrap();
    assert_eq!(*peer.states().borrow(), PeerState::Closed);
    assert_eq!(peer.send([0; 320]), Err(crate::Error::Closed));
    assert_eq!(peer.recv().await, Ok(None));
    peer.close().await.unwrap();
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
    let udp = peer.runtime.udp.lock().unwrap().clone();
    let tcp = peer.runtime.tcp.lock().unwrap().clone();
    assert_eq!(owned_sockets(&peer), sockets);
    peer.close().await.unwrap();
    assert!(peer.shared.tasks.lock().unwrap().is_empty());
    assert!(udp.iter().all(|socket| socket.upgrade().is_none()));
    assert!(tcp.iter().all(|socket| socket.upgrade().is_none()));
}

#[tokio::test]
async fn t3_media_socket_witness_tracks_retained_actual_owners() {
    let mut peer = Peer::create(config()).await.unwrap();
    peer.offer().await.unwrap();
    let udp = peer.runtime.udp.lock().unwrap()[0].clone();
    let tcp = peer.runtime.tcp.lock().unwrap()[0].clone();
    let retained_udp = udp.upgrade().unwrap();
    let retained_tcp = tcp.upgrade().unwrap();
    peer.close().await.unwrap();
    assert!(udp.upgrade().is_some());
    assert!(tcp.upgrade().is_some());
    drop((retained_udp, retained_tcp));
    assert!(udp.upgrade().is_none());
    assert!(tcp.upgrade().is_none());
    peer.close().await.unwrap();
    assert!(udp.upgrade().is_none());
    assert!(tcp.upgrade().is_none());
}

fn owned_sockets(peer: &Peer) -> std::collections::BTreeSet<(String, IpAddr, u16)> {
    let mut owned = std::collections::BTreeSet::new();
    for socket in peer.runtime.udp.lock().unwrap().iter() {
        let address = socket.upgrade().unwrap().local_addr().unwrap();
        owned.insert(("udp".to_owned(), address.ip(), address.port()));
    }
    for socket in peer.runtime.tcp.lock().unwrap().iter() {
        let address = socket.upgrade().unwrap().local_addr().unwrap();
        owned.insert(("tcp".to_owned(), address.ip(), address.port()));
    }
    owned
}
