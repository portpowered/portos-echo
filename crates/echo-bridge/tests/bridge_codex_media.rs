#![cfg(test)]
//! T3 component: real echo-media versus the existing local fake's str0m peer.
//! Controlled signaling and real pinned-server signaling; neither is a milestone witness.
#[path = "pinned_media/mod.rs"]
mod pinned_media;
use echo_bridge::bridge_codex::{CaptureAudio, Event, MediaCall, MediaEvent, Options, ReplyAudio};
use echo_codex::{codex_io::RpcIo, codex_rpc::Limits};
use echo_live::LivePeer;
use echo_media::{DropCounts, MediaPeer, Peer, PeerConfig};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, Ipv4Addr},
    num::NonZeroUsize,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    time::Instant,
};

type Server = BufReader<DuplexStream>;

struct MediaClock(tokio::task::JoinHandle<()>);
impl MediaClock {
    fn start() -> Self {
        tokio::time::pause();
        // Like echo-media's tone fixture: readiness may await OS packets, so
        // disable automatic jumps; only the explicit source steps advance time.
        Self(tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        }))
    }
}
impl Drop for MediaClock {
    fn drop(&mut self) {
        self.0.abort();
        tokio::time::resume();
    }
}
async fn read(server: &mut Server) -> Value {
    let mut line = String::new();
    assert_ne!(server.read_line(&mut line).await.unwrap(), 0);
    serde_json::from_str(&line).unwrap()
}
async fn write(server: &mut Server, value: Value) {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    server.get_mut().write_all(&bytes).await.unwrap();
}
async fn signal(server: &mut Server, peer: &mut LivePeer) {
    let request = read(server).await;
    assert_eq!(request["method"], "thread/start");
    assert_eq!(request["params"]["dynamicTools"], json!([]));
    write(
        server,
        json!({"id":request["id"],"result":{"thread":{"id":"media-thread"}}}),
    )
    .await;
    let request = read(server).await;
    assert_eq!(request["method"], "thread/realtime/start");
    assert_eq!(request["params"]["version"], "v3");
    let offer = request["params"]["transport"]["sdp"].as_str().unwrap();
    let answer = peer.answer(offer).await.unwrap();
    write(
        server,
        json!({"method":"thread/realtime/sdp","params":{"threadId":"media-thread","sdp":answer}}),
    )
    .await;
    write(server, json!({"id":request["id"],"result":{}})).await;
}
async fn step(peer: &mut LivePeer) {
    let next = peer.flush().await.unwrap();
    tokio::select! {
        result = peer.readable() => { result.unwrap(); peer.receive(Instant::now().into_std()).await.unwrap(); },
        () = tokio::time::sleep_until(next.into()) => peer.timeout(Instant::now().into_std()).await.unwrap(),
    }
}
async fn remote(server: &mut Server, peer: &mut LivePeer) -> usize {
    signal(server, peer).await;
    while !peer.ready() {
        step(peer).await;
    }
    let mut echoed = 0;
    loop {
        while let Some(frame) = peer.pop_audio() {
            peer.send_audio(frame, Instant::now().into_std()).await.unwrap();
            echoed += 1;
        }
        tokio::select! {
            request = read(server) => {
                assert_eq!(request["method"], "thread/realtime/stop");
                assert_eq!(request["params"]["threadId"], "media-thread");
                write(server, json!({"id":request["id"],"result":{}})).await;
                write(server, json!({"method":"thread/realtime/closed","params":{"threadId":"media-thread"}})).await;
                return echoed;
            },
            () = step(peer) => (),
        }
    }
}

#[tokio::test]
async fn t3_codex_media_negotiates_real_sdp_and_round_trips_pcm_over_fake_peer() {
    tokio::time::timeout(Duration::from_secs(20), Box::pin(round_trip()))
        .await
        .unwrap();
}
async fn round_trip() {
    let config = PeerConfig {
        interface_allowlist: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        allow_loopback: true,
        queue_capacity: NonZeroUsize::new(32).unwrap(),
        ..PeerConfig::default()
    };
    let peer = Peer::create(config).await.unwrap();
    let (client, server) = tokio::io::duplex(65_536);
    let (reader, writer) = tokio::io::split(client);
    let mut io = RpcIo::new(reader, writer, Limits::default()).unwrap();
    let mut server = BufReader::new(server);
    let mut fake = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        let options = Options {
            voice: Some("alloy"),
            instructions: "Client audio bridge",
            prompt: "Client audio bridge",
            model: None,
            effort: None,
            time_zone: None,
            web_search: echo_config::WebSearch::default(),
        };
        call.start(&options, vec![], Instant::now() + Duration::from_secs(10))
            .await
            .unwrap();
        audio_round_trip(&mut call).await;
        call.close(Instant::now() + Duration::from_secs(5)).await.unwrap();
    };
    let ((), echoed) = tokio::join!(client, remote(&mut server, &mut fake));
    assert!((4..=8).contains(&echoed), "fake peer echoed {echoed} Opus packets");
    fake.close();
    assert!(io.begin_request("account/read", &json!({})).is_ok());
}

async fn audio_round_trip<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    call: &mut MediaCall<'_, R, W>,
) {
    let clock = MediaClock::start();
    // A fixed PCM burst exercises framing/codec interoperability, not pacing.
    let waveform: [i16; 16] = [
        0, 6000, 11000, 14500, 16000, 14500, 11000, 6000, 0, -6000, -11000, -14500, -16000, -14500, -11000, -6000,
    ];
    let format = echo_bridge::bridge_audio::format(16_000);
    let mut capture = CaptureAudio::new(format).unwrap();
    for _ in 0..8 {
        tokio::time::advance(Duration::from_millis(20)).await;
        let bytes: Vec<_> = (0..320).flat_map(|n| waveform[n % 16].to_le_bytes()).collect();
        for chunk in bytes.chunks(146) {
            for frame in capture.process(&format, chunk).unwrap() {
                call.send(frame).unwrap();
            }
        }
    }
    assert_eq!(capture.finish().unwrap(), Vec::<echo_media::PcmFrame>::new());
    assert_eq!(capture.tail_samples(), 0);
    let mut reply = ReplyAudio::new(200).unwrap();
    let mut samples = Vec::new();
    while samples.len() < 3 * 320 {
        match call.next().await.unwrap() {
            MediaEvent::Audio(frame) => {
                reply.push(&frame).unwrap();
                samples.extend(*frame);
            }
            MediaEvent::Control(event) => assert!(matches!(event, Event::Other)),
        }
    }
    let energy: i64 = samples.iter().map(|s| i64::from(*s).pow(2)).sum();
    assert!(energy > 960 * 1000_i64.pow(2), "round-trip PCM energy {energy}");
    check_reply(&mut reply).await;
    assert_eq!(call.dropped_frames(), DropCounts::default());
    drop(clock);
}

async fn check_reply(reply: &mut ReplyAudio) {
    // Only this accepted three-frame component stream is finished here.
    // Full reply completion still requires the run owner's media/played proof.
    reply.finish().unwrap();
    let mut pcm = Vec::new();
    while let Some(frame) = reply.next_frame().await {
        assert_eq!(frame.len(), 960);
        pcm.extend(frame);
    }
    assert_eq!(pcm.len(), 3 * 960);
    let pcm_energy: i64 = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i64::from(i16::from_le_bytes([b[0], b[1]])).pow(2))
        .sum();
    assert!(pcm_energy > 1440 * 1000_i64.pow(2), "Wyoming PCM energy {pcm_energy}");
    assert!(reply.drained());
    assert_eq!(reply.dropped_frames(), 0);
}
