// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Modified by Port OS: loopback-only optional STUN behavior.
use crate::{MediaPeer, Peer};
use rtc::stun::{
    message::{BINDING_SUCCESS, Message},
    xoraddr::XorMappedAddress,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::net::UdpSocket;

#[tokio::test]
async fn t3_peer_stun_optional_local_fake_and_default_zero_requests() {
    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let addr = socket.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let server = socket.clone();
    let requests = count.clone();
    let worker = tokio::spawn(async move {
        let mut buffer = [0; 2048];
        loop {
            let (len, from) = server.recv_from(&mut buffer).await.unwrap();
            let mut request = Message::new();
            request.unmarshal_binary(&buffer[..len]).unwrap();
            requests.fetch_add(1, Ordering::SeqCst);
            let mut response = Message::new();
            response
                .build(&[
                    Box::new(BINDING_SUCCESS),
                    Box::new(request.transaction_id),
                    Box::new(XorMappedAddress {
                        ip: from.ip(),
                        port: from.port(),
                    }),
                ])
                .unwrap();
            server.send_to(&response.marshal_binary().unwrap(), from).await.unwrap();
        }
    });
    let mut peer = Peer::create(super::config()).await.unwrap();
    peer.offer().await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    peer.close().await.unwrap();
    let mut config = super::config();
    config.stun_servers = vec![format!("stun:{addr}")];
    let mut peer = Peer::create(config).await.unwrap();
    let offer = peer.offer().await.unwrap();
    assert!(
        offer
            .lines()
            .any(|line| line.starts_with("a=candidate:") && line.contains(" typ srflx"))
    );
    assert!(count.load(Ordering::SeqCst) >= 1);
    peer.close().await.unwrap();
    worker.abort();
    let _ = worker.await;
    // The joined worker no longer holds its socket. This witness tracks this
    // exact owner even if another parallel test immediately reuses its address.
    assert_eq!(Arc::strong_count(&socket), 1);
    let released = Arc::downgrade(&socket);
    assert!(released.upgrade().is_some());
    drop(socket);
    assert!(released.upgrade().is_none());
}

#[tokio::test(start_paused = true)]
async fn t3_peer_stun_silent_fake_is_bounded() {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut config = super::config();
    config.stun_servers = vec![format!("stun:{}", socket.local_addr().unwrap())];
    config.negotiation_timeout = Duration::from_millis(100);
    let mut peer = Peer::create(config).await.unwrap();
    assert!(peer.offer().await.is_err());
    assert_eq!(*peer.states().borrow(), crate::PeerState::Failed);
    peer.close().await.unwrap();
    assert_eq!(peer.recv().await, Ok(None));
}
