use super::{FRAME_TIME, FRAMES, ResultRow, WARM, core::Witness, measure};
use echo_audio::{OpusDecoder, OpusEncoder};
use echo_live::{AudioFrame, LivePeer};
use echo_media::{DropCounts, MediaPeer, PcmFrame, Peer, PeerConfig, PeerState};
use std::{net::Ipv4Addr, time::Duration};
use tokio::{runtime::Runtime, time::Instant};

pub(super) struct Media {
    peer: Peer,
    remote: LivePeer,
    uplink_decoder: OpusDecoder,
    uplink_pcm: [i16; 960],
    previous: Option<(u16, u32)>,
    next_downlink: u16,
    downlink_packet: Vec<u8>,
}

impl Media {
    pub(super) async fn open() -> Self {
        // Negotiate before sampling, using the normal timer/readiness race.
        tokio::time::resume();
        let (peer, remote) = tokio::time::timeout(Duration::from_secs(10), negotiate())
            .await
            .unwrap();
        tokio::time::pause();
        let mut encoder = OpusEncoder::new().unwrap();
        let mut packet = [0; 1275];
        let input = std::array::from_fn::<_, 960, _>(|index| tone(index / 3));
        let mut len = 0;
        for _ in 0..WARM {
            len = encoder.encode(&input, &mut packet).unwrap();
        }
        Self {
            peer,
            remote,
            uplink_decoder: OpusDecoder::new().unwrap(),
            uplink_pcm: [0; 960],
            previous: None,
            next_downlink: 0,
            downlink_packet: packet[..len].to_vec(),
        }
    }

    pub(super) fn rows(&mut self, runtime: &Runtime, world: &Witness) -> [ResultRow; 2] {
        // The production converters hold the first incomplete output block.
        let frame: PcmFrame = std::array::from_fn(tone);
        assert!(self.peer.try_send(frame).unwrap());
        runtime.block_on(self.uplink(world, &frame, WARM));
        let up = measure(
            runtime,
            world,
            "uplink",
            "WebRTC+Tokio+Opus+loopback-peer",
            self.uplink(world, &frame, FRAMES),
        );
        runtime.block_on(self.send_downlink());
        runtime.block_on(self.downlink(world, WARM));
        let down = measure(
            runtime,
            world,
            "downlink",
            "WebRTC+Tokio+Opus+loopback-peer",
            self.downlink(world, FRAMES),
        );
        assert_eq!(self.peer.dropped_frames(), DropCounts::default());
        assert_eq!(self.remote.dropped_audio(), 0);
        [up, down]
    }

    async fn uplink(&mut self, world: &Witness, frame: &PcmFrame, count: usize) {
        let mut energy = 0;
        for _ in 0..count {
            let (attempt, payload) = world.offer_uplink(frame);
            assert_eq!(&payload, frame);
            assert!(self.peer.try_send(payload).unwrap());
            world.accepted(attempt);
            let packet = self.next_uplink().await;
            if let Some((sequence, timestamp)) = self.previous.replace((packet.sequence, packet.timestamp)) {
                assert_eq!(packet.sequence, sequence.wrapping_add(1));
                assert_eq!(packet.timestamp, timestamp.wrapping_add(960));
            }
            assert!((1..=1275).contains(&packet.payload.len()));
            assert_eq!(
                self.uplink_decoder
                    .decode(&packet.payload, &mut self.uplink_pcm)
                    .unwrap(),
                960
            );
            energy += amplitude(&self.uplink_pcm);
            world.advance(FRAME_TIME);
            tokio::time::advance(FRAME_TIME).await;
        }
        assert!(energy / u64::try_from(count).unwrap() > 1_000, "uplink lost the tone");
    }

    async fn next_uplink(&mut self) -> AudioFrame {
        loop {
            if let Some(audio) = self.remote.pop_audio() {
                return audio;
            }
            drive(&mut self.remote).await;
        }
    }

    async fn send_downlink(&mut self) {
        let sequence = self.next_downlink;
        self.next_downlink += 1;
        self.remote
            .send_audio(
                AudioFrame {
                    sequence,
                    timestamp: u32::from(sequence) * 960,
                    payload: self.downlink_packet.clone(),
                },
                Instant::now().into_std(),
            )
            .await
            .unwrap();
    }

    async fn downlink(&mut self, world: &Witness, count: usize) {
        let mut energy = 0;
        for _ in 0..count {
            self.send_downlink().await;
            let frame = self.peer.recv().await.unwrap().unwrap();
            energy += amplitude(&frame);
            world.receive(&frame);
            world.advance(FRAME_TIME);
            tokio::time::advance(FRAME_TIME).await;
        }
        assert!(energy / u64::try_from(count).unwrap() > 1_000, "downlink lost the tone");
    }

    pub(super) async fn close(&mut self) {
        tokio::time::resume();
        self.peer.close().await.unwrap();
        self.remote.close();
        tokio::time::pause();
    }
}

async fn negotiate() -> (Peer, LivePeer) {
    let peer = Peer::create(PeerConfig {
        interface_allowlist: vec![Ipv4Addr::LOCALHOST.into()],
        allow_loopback: true,
        ..PeerConfig::default()
    })
    .await
    .unwrap();
    let offer = peer.offer().await.unwrap();
    let mut remote = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let answer = remote.answer(&offer).await.unwrap();
    peer.apply_answer(&answer).await.unwrap();
    let mut states = peer.states();
    let ready = async {
        while !remote.ready() {
            drive(&mut remote).await;
        }
    };
    let (connected, ()) = tokio::join!(states.wait_for(|state| *state != PeerState::Connecting), ready);
    assert_eq!(*connected.unwrap(), PeerState::Connected);
    (peer, remote)
}

async fn drive(peer: &mut LivePeer) {
    let deadline = peer.flush().await.unwrap();
    let readable = tokio::select! {
        result = peer.readable() => { result.unwrap(); true }
        () = tokio::time::sleep_until(deadline.into()) => false,
    };
    let now = Instant::now().into_std();
    if readable {
        peer.receive(now).await.unwrap();
    } else {
        peer.timeout(now).await.unwrap();
    }
}

fn tone(index: usize) -> i16 {
    const SIGNAL: [i16; 16] = [
        0, 6000, 11000, 14500, 16000, 14500, 11000, 6000, 0, -6000, -11000, -14500, -16000, -14500, -11000, -6000,
    ];
    SIGNAL[index % SIGNAL.len()]
}

fn amplitude(samples: &[i16]) -> u64 {
    let sum: u64 = samples.iter().map(|s| u64::from(s.unsigned_abs())).sum();
    sum / u64::try_from(samples.len()).unwrap()
}
