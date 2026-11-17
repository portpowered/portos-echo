// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host transport.rs; Modified by Port OS.
use crate::{Error, PeerConfig, media_state::Shared};
use std::{
    collections::HashSet,
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
};
use tokio::sync::Notify;
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    peer_connection::{
        PeerConnectionEventHandler, RTCIceCandidateInit, RTCIceGatheringState, RTCPeerConnectionState,
        RTCSessionDescription,
    },
};
type BoxFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

pub(crate) fn admits(config: &PeerConfig, ip: IpAddr) -> bool {
    !ip.is_unspecified()
        && !ip.is_multicast()
        && match ip {
            IpAddr::V4(v) => !v.is_link_local(),
            IpAddr::V6(v) => !v.is_unicast_link_local(),
        }
        && (!ip.is_loopback() || config.allow_loopback)
        && (config.interface_allowlist.is_empty() || config.interface_allowlist.contains(&ip))
}

pub(crate) fn addresses(config: &PeerConfig, inventory: impl Iterator<Item = IpAddr>) -> Vec<SocketAddr> {
    let mut ips: Vec<_> = inventory.filter(|ip| admits(config, *ip)).collect();
    ips.sort_unstable();
    ips.dedup();
    ips.into_iter().map(|ip| SocketAddr::new(ip, 0)).collect()
}

pub(crate) fn validate(config: &PeerConfig) -> Result<(), Error> {
    if config.negotiation_timeout.is_zero()
        || config.negotiation_timeout.as_secs() > 300
        || config.queue_capacity.get() > 1000
        || config.stun_servers.len() > 8
        || config
            .stun_servers
            .iter()
            .any(|s| !s.starts_with("stun:") || s.len() > 256 || s.contains('@'))
        || config.interface_allowlist.iter().any(|ip| !admits(config, *ip))
    {
        return Err(Error::InvalidConfig);
    }
    Ok(())
}

pub(crate) fn remote(sdp: &str, answer: bool) -> Result<(RTCSessionDescription, Vec<RTCIceCandidateInit>), Error> {
    if sdp.is_empty() || sdp.len() > 64 * 1024 {
        return Err(Error::InvalidSdp);
    }
    let description = if answer {
        RTCSessionDescription::answer(sdp.into())
    } else {
        RTCSessionDescription::offer(sdp.into())
    }
    .map_err(|_| Error::InvalidSdp)?;
    let parsed = description.unmarshal().map_err(|_| Error::InvalidSdp)?;
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for (index, attr) in parsed
        .media_descriptions
        .iter()
        .flat_map(|m| &m.attributes)
        .filter(|a| a.key == "candidate")
        .enumerate()
    {
        if index >= 32 {
            return Err(Error::InvalidSdp);
        }
        let text = attr.value.as_deref().ok_or(Error::InvalidSdp)?;
        let candidate = rtc::ice::candidate::unmarshal_candidate(text).map_err(|_| Error::InvalidSdp)?;
        if candidate.component() == 1 && seen.insert(text) {
            candidates.push(RTCIceCandidateInit {
                candidate: format!("candidate:{text}"),
                ..Default::default()
            });
        }
    }
    Ok((description, candidates))
}

pub(crate) struct Events {
    pub(crate) shared: Arc<Shared>,
    pub(crate) gathered: Arc<Notify>,
    pub(crate) answerer: bool,
}
impl PeerConnectionEventHandler for Events {
    fn on_ice_gathering_state_change<'a, 'b>(&'a self, state: RTCIceGatheringState) -> BoxFuture<'b>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async move {
            if state == RTCIceGatheringState::Complete {
                self.gathered.notify_one();
            }
        })
    }
    fn on_connection_state_change<'a, 'b>(&'a self, state: RTCPeerConnectionState) -> BoxFuture<'b>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async move {
            match state {
                RTCPeerConnectionState::Connected => self.shared.connected(true),
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Disconnected => {
                    self.shared.fail(Error::Transport);
                }
                _ => {}
            }
        })
    }
    fn on_data_channel<'a, 'b>(&'a self, channel: Arc<dyn DataChannel>) -> BoxFuture<'b>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async move {
            if self.answerer && channel.label().await.ok().as_deref() == Some("oai-events") {
                observe(&self.shared, channel);
            } else {
                let _ = channel.close().await;
            }
        })
    }
}

pub(crate) fn observe(shared: &Arc<Shared>, channel: Arc<dyn DataChannel>) {
    let state = shared.clone();
    shared.task(tokio::spawn(async move {
        while let Some(event) = channel.poll().await {
            match event {
                DataChannelEvent::OnOpen => state.connected(false),
                DataChannelEvent::OnError | DataChannelEvent::OnClosing | DataChannelEvent::OnClose => break,
                _ => {}
            }
        }
        state.fail(Error::Transport);
    }));
}
