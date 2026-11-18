// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host transport.rs; Modified by Port OS.
use crate::{
    DropCounts, Error, MediaPeer, PcmFrame, PeerConfig, PeerState,
    media_audio::{Decoder, Encoder},
    media_ingress::Ingress,
    media_signal::{self, Events},
    media_state::Shared,
    media_track::AudioTrack,
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::Notify, time::timeout};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, RTCConfigurationBuilder, RTCIceServer, SettingEngineBuilder,
};

type ReceiveAck = (tokio::time::Instant, tokio::sync::oneshot::Receiver<Result<(), Error>>);

/// One caller-owned peer. Call close to await resource release; Drop cancels workers.
pub struct Peer {
    pub(crate) shared: Arc<Shared>,
    connection: Option<Arc<dyn PeerConnection>>,
    gathered: Arc<Notify>,
    config: PeerConfig,
    reset_ack: Option<ReceiveAck>,
    #[cfg(test)]
    pub(crate) runtime: Arc<crate::media_runtime::MediaRuntime>,
}
impl Peer {
    pub(crate) async fn build(config: PeerConfig, answerer: bool) -> Result<Self, Error> {
        media_signal::validate(&config)?;
        let inventory = rtc::shared::ifaces::ifaces().map_err(|_| Error::Transport)?;
        let addresses = media_signal::addresses(&config, inventory.into_iter().filter_map(|i| i.addr.map(|a| a.ip())));
        if addresses.is_empty() {
            return Err(Error::InvalidConfig);
        }
        let shared = Arc::new(Shared::new(config.queue_capacity.get()));
        let gathered = Arc::new(Notify::new());
        let (media, track) = AudioTrack::new()?;
        let settings = settings(&config);
        let servers = config
            .stun_servers
            .iter()
            .map(|url| RTCIceServer {
                urls: vec![url.clone()],
                ..Default::default()
            })
            .collect();
        let runtime = Arc::new(crate::media_runtime::MediaRuntime::default());
        let connection: Arc<dyn PeerConnection> = Arc::new(
            Box::pin(
                PeerConnectionBuilder::new()
                    .with_runtime(runtime.clone())
                    .with_configuration(RTCConfigurationBuilder::new().with_ice_servers(servers).build())
                    .with_media_engine(media)
                    .with_setting_engine(settings)
                    .with_interceptor_registry(rtc::interceptor::Registry::new().with(
                        rtc::interceptor::Slot::Custom(14000),
                        Ingress::new(shared.ingress.clone()),
                    ))
                    .with_handler(Arc::new(Events {
                        shared: shared.clone(),
                        gathered: gathered.clone(),
                        answerer,
                    }))
                    .with_udp_addrs(addresses.clone())
                    .with_tcp_addrs(addresses)
                    .with_data_channel_send_buffer_limit(64 * 1024)
                    .build(),
            )
            .await
            .map_err(|_| Error::Transport)?,
        );
        let peer = Self {
            shared,
            connection: Some(connection),
            gathered,
            config,
            reset_ack: None,
            #[cfg(test)]
            runtime,
        };
        if let Err(error) = peer.attach(track, answerer).await {
            let _ = peer.connection()?.close().await;
            return Err(error);
        }
        Ok(peer)
    }

    async fn attach(&self, track: AudioTrack, answerer: bool) -> Result<(), Error> {
        self.connection()?
            .add_track(track.track.clone())
            .await
            .map_err(|_| Error::Transport)?;
        if !answerer {
            let channel = self
                .connection()?
                .create_data_channel("oai-events", None)
                .await
                .map_err(|_| Error::Transport)?;
            media_signal::observe(&self.shared, channel);
        }
        let mut encoder = Encoder::new()?;
        let state = self.shared.clone();
        self.shared.task(tokio::spawn(async move {
            while let Some(frame) = state.send.pop().await {
                let compression = encoder.encode(&frame);
                #[cfg(test)]
                let emitted = compression.as_ref().is_ok_and(Option::is_some);
                let result = match compression {
                    Ok(Some(data)) => track.send(data).await,
                    Ok(None) => Ok(()),
                    Err(e) => Err(e),
                };
                if let Err(error) = result {
                    state.fail(error);
                    break;
                }
                #[cfg(test)]
                state.sent.send_modify(|p| {
                    p.0 += 1;
                    p.1 += u32::from(emitted);
                });
            }
        }));
        let decoder = Decoder::new()?;
        let state = self.shared.clone();
        self.shared
            .task(tokio::spawn(async move { state.receive(decoder).await }));
        Ok(())
    }

    fn connection(&self) -> Result<&Arc<dyn PeerConnection>, Error> {
        self.connection.as_ref().ok_or(Error::Closed)
    }

    #[cfg(test)]
    pub(crate) fn controlled() -> Self {
        Self {
            shared: Arc::new(Shared::new(3)),
            connection: None,
            gathered: Arc::new(Notify::new()),
            config: PeerConfig::default(),
            reset_ack: None,
            #[cfg(test)]
            runtime: Arc::new(crate::media_runtime::MediaRuntime::default()),
        }
    }

    pub(crate) fn begin(&self) -> Result<(), Error> {
        self.shared.begin()?;
        let shared = self.shared.clone();
        let duration = self.config.negotiation_timeout;
        let connection = self.connection()?.clone();
        self.shared.task(tokio::spawn(async move {
            let mut states = shared.state.subscribe();
            if timeout(duration, states.wait_for(|s| *s != PeerState::Connecting))
                .await
                .is_err()
            {
                shared.fail(Error::Deadline);
                let _ = connection.close().await;
            }
        }));
        Ok(())
    }

    pub(crate) async fn local(&self, answer: bool) -> Result<String, Error> {
        let description = if answer {
            self.connection()?.create_answer(None).await
        } else {
            self.connection()?.create_offer(None).await
        }
        .map_err(|_| Error::Transport)?;
        self.connection()?
            .set_local_description(description)
            .await
            .map_err(|_| Error::Transport)?;
        timeout(self.config.negotiation_timeout, self.gathered.notified())
            .await
            .map_err(|_| Error::Deadline)?;
        self.shared.check()?;
        self.connection()?
            .local_description()
            .await
            .map(|d| d.sdp)
            .ok_or(Error::Transport)
    }

    pub(crate) async fn remote(&self, sdp: &str, answer: bool) -> Result<(), Error> {
        self.shared.check()?;
        let (description, candidates) = media_signal::remote(sdp, answer)?;
        self.connection()?
            .set_remote_description(description)
            .await
            .map_err(|_| Error::InvalidSdp)?;
        for candidate in candidates {
            self.connection()?
                .add_ice_candidate(candidate)
                .await
                .map_err(|_| Error::Transport)?;
        }
        Ok(())
    }
}

impl MediaPeer for Peer {
    async fn create(config: PeerConfig) -> Result<Self, Error> {
        Self::build(config, false).await
    }
    async fn offer(&self) -> Result<String, Error> {
        self.begin()?;
        let result = self.local(false).await;
        if let Err(error) = result {
            self.shared.fail(error);
        }
        result
    }
    async fn apply_answer(&self, sdp: &str) -> Result<(), Error> {
        self.shared.check()?;
        if *self.shared.state.borrow() != PeerState::Connecting {
            return Err(Error::State);
        }
        self.remote(sdp, true).await
    }
    fn states(&self) -> tokio::sync::watch::Receiver<PeerState> {
        self.shared.state.subscribe()
    }
    fn observe_setup(&self, observer: crate::SetupObserver) -> Result<(), Error> {
        self.shared.observe_setup(observer)
    }
    fn send(&self, frame: PcmFrame) -> Result<(), Error> {
        self.shared.check()?;
        if *self.shared.state.borrow() != PeerState::Connected {
            return Err(Error::NotConnected);
        }
        self.shared.send.push(frame);
        Ok(())
    }
    async fn recv(&mut self) -> Result<Option<PcmFrame>, Error> {
        let frame = self.shared.recv.pop().await;
        if *self.shared.state.borrow() == PeerState::Closed {
            return Ok(None);
        }
        self.shared.check()?;
        Ok(frame)
    }
    async fn reset_receive(&mut self) -> Result<(), Error> {
        self.shared.check()?;
        if *self.shared.state.borrow() != PeerState::Connected {
            return Err(Error::NotConnected);
        }
        if self.reset_ack.is_none() {
            let (ack, received) = tokio::sync::oneshot::channel();
            self.shared.resets.push(ack);
            self.reset_ack = Some((tokio::time::Instant::now() + Duration::from_secs(2), received));
        }
        let (deadline, received) = self.reset_ack.as_mut().ok_or(Error::State)?;
        let result = tokio::time::timeout_at(*deadline, received)
            .await
            .map_err(|_| Error::Deadline)?;
        self.reset_ack = None;
        self.shared.check()?;
        result.map_err(|_| Error::Closed)?
    }
    fn dropped_frames(&self) -> DropCounts {
        DropCounts {
            send: self.shared.send.drops(),
            recv: self.shared.recv.drops() + self.shared.ingress.drops(),
        }
    }
    async fn close(&mut self) -> Result<(), Error> {
        self.shared.close();
        let tasks = std::mem::take(
            &mut *self
                .shared
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        let Some(connection) = self.connection.take() else {
            return Ok(());
        };
        timeout(Duration::from_secs(2), connection.close())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Transport)
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.shared.close();
        for task in self
            .shared
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            task.abort();
        }
        let Some(connection) = self.connection.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = connection.close().await;
            });
        }
    }
}

fn settings(config: &PeerConfig) -> webrtc::peer_connection::SettingEngine {
    use rtc::ice::mdns::MulticastDnsMode;

    // Explicit loopback transports cannot need remote .local resolution. Avoid
    // opening a multicast socket on hosts with no multicast-capable interface.
    let mdns = if !config.interface_allowlist.is_empty()
        && config.interface_allowlist.iter().all(std::net::IpAddr::is_loopback)
    {
        MulticastDnsMode::Disabled
    } else {
        MulticastDnsMode::QueryOnly
    };
    let attempts = u16::try_from(config.negotiation_timeout.as_millis() / 200)
        .unwrap_or(u16::MAX)
        .max(10);
    SettingEngineBuilder::new()
        .with_multicast_dns_mode(mdns)
        .with_include_loopback_candidate(config.allow_loopback)
        .with_sctp_max_receive_buffer_size(64 * 1024)
        .with_ice_connection_attempts(Some(Duration::from_millis(200)), Some(attempts))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtc::ice::mdns::MulticastDnsMode::{Disabled, QueryOnly};

    #[test]
    fn mdns_policy_requires_explicit_entirely_loopback_allowlist() {
        for (addresses, expected) in [
            (vec![], QueryOnly),
            (vec!["127.0.0.1"], Disabled),
            (vec!["127.0.0.2"], Disabled),
            (vec!["::1"], Disabled), // lint-rules: allow-literal inert socket-free settings assertion
            (vec!["127.0.0.1", "::1"], Disabled), // lint-rules: allow-literal inert socket-free settings assertion
            (vec!["192.0.2.10"], QueryOnly), // lint-rules: allow-literal inert socket-free settings assertion
            (vec!["127.0.0.1", "192.0.2.10"], QueryOnly), // lint-rules: allow-literal inert socket-free settings assertion
        ] {
            for allow_loopback in [false, true] {
                let config = PeerConfig {
                    interface_allowlist: addresses.iter().map(|a| a.parse().unwrap()).collect(),
                    allow_loopback,
                    ..PeerConfig::default()
                };
                assert_eq!(settings(&config).multicast_dns().mode, expected);
            }
        }
    }
}
