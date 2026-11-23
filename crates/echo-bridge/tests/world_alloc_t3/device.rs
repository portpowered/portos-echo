use super::{FRAME_TIME, FRAMES, ResultRow, WARM, core::Witness, measure, screen::Screen};
use echo_bridge::bridge_codex::CaptureAudio;
use echo_contracts::wyoming::{Event, Header, encode};
use echo_core_api::{PacerEvent, ReplyFrame, SessionEvent};
use echo_link::{EchoLink, LinkEvent, LinkEvents, MicrophoneRun, PcmFormat, PlayOutcome, ReplySink};
use echo_wyoming::{ClientOptions, Run, WyomingClient, adapter::Events};
use serde_json::{Value, json};
use std::{net::Ipv4Addr, num::NonZeroUsize, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    runtime::Runtime,
};

const CAPTURE: PcmFormat = PcmFormat {
    rate: 16_000,
    width: 2,
    channels: 1,
};
const REPLY: PcmFormat = PcmFormat {
    rate: 24_000,
    ..CAPTURE
};

pub(super) struct Device {
    socket: BufReader<TcpStream>,
    link: WyomingClient,
    events: Events,
    pipeline: Vec<u8>,
    capture: Vec<u8>,
    pcm: [u8; 640],
}

impl Device {
    pub(super) async fn open(world: Arc<Witness>) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let connect =
            WyomingClient::connect_endpoint(listener.local_addr().unwrap().into(), ClientOptions::default(), world);
        let (link, accepted) = tokio::join!(connect, listener.accept());
        let mut link = link.unwrap();
        let events = EchoLink::events(&mut link).unwrap();
        let (socket, _) = accepted.unwrap();
        socket.set_nodelay(true).unwrap();
        let mut socket = BufReader::new(socket);
        let mut registration = Vec::new();
        let start = link.enable_capture(30);
        let read = tokio::io::AsyncBufReadExt::read_until(&mut socket, b'\n', &mut registration);
        let (started, size) = tokio::join!(start, read);
        started.unwrap();
        assert!(size.unwrap() < 512);
        let header: Header = serde_json::from_slice(&registration).unwrap();
        assert_eq!(header.r#type, "run-satellite");
        let mut segment = vec![0; header.data_length];
        socket.read_exact(&mut segment).await.unwrap();
        let pcm: [u8; 640] = [0x00, 0x10].repeat(320).try_into().unwrap();
        Self {
            socket,
            link,
            events,
            pcm,
            pipeline: wire(
                "run-pipeline",
                json!({
                    "start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"synthetic"
                }),
                vec![],
            ),
            capture: wire(
                "audio-chunk",
                json!({"rate":16000,"channels":1,"width":2}),
                pcm.to_vec(),
            ),
        }
    }

    async fn wake(&mut self) -> Run {
        self.socket.get_mut().write_all(&self.pipeline).await.unwrap();
        match self.events.recv().await.unwrap() {
            Some(LinkEvent::Wake(run)) => run,
            _ => panic!("missing microphone run"),
        }
    }

    async fn capture_frames(&mut self, world: &Witness, run: &mut Run, capture: &mut CaptureAudio, count: usize) {
        let mut scratch = Box::new([0; 320]);
        for _ in 0..count {
            self.socket.get_mut().write_all(&self.capture).await.unwrap();
            let frame = MicrophoneRun::recv(run).await.unwrap().unwrap();
            assert_eq!(frame.format, CAPTURE);
            assert_eq!(&*frame.payload, &self.pcm);
            let samples = capture.process(&frame.format, &frame.payload).unwrap();
            assert_eq!(samples, &[[4096; 320]]);
            *scratch = samples[0];
            scratch = world.microphone(scratch);
            world.advance(FRAME_TIME);
            assert!(matches!(self.events.recv().await.unwrap(), Some(LinkEvent::Other)));
        }
        assert_eq!(world.microphone_peak(), 4096);
    }

    pub(super) fn capture_rows(&mut self, runtime: &Runtime, world: &Witness) -> [ResultRow; 2] {
        let mut run = runtime.block_on(self.wake());
        let mut capture = CaptureAudio::new(CAPTURE).unwrap();
        runtime.block_on(self.capture_frames(world, &mut run, &mut capture, WARM));
        let capture_row = measure(
            runtime,
            world,
            "capture",
            "Wyoming+Tokio+capture-probe",
            self.capture_frames(world, &mut run, &mut capture, FRAMES),
        );
        assert_eq!(run.stats().received, u64::try_from(WARM + FRAMES).unwrap());
        assert_eq!(run.stats().dropped, 0);
        assert!(capture.finish().unwrap().is_empty());
        assert_eq!(capture.tail_samples(), 0);
        let mute = wire("hassmic-mute", json!({"muted":true}), vec![]);
        let unmute = wire("hassmic-mute", json!({"muted":false}), vec![]);
        runtime.block_on(self.mute_frames(world, &mute, &unmute, WARM));
        let mute_row = measure(
            runtime,
            world,
            "mute",
            "Wyoming+Tokio+run-boundaries",
            self.mute_frames(world, &mute, &unmute, FRAMES),
        );
        drop(run);
        let _run = runtime.block_on(self.wake());
        assert!(world.device_view().capture_open);
        [capture_row, mute_row]
    }

    async fn mute_frames(&mut self, world: &Witness, mute: &[u8], unmute: &[u8], count: usize) {
        for _ in 0..count {
            let mut run = self.wake().await;
            self.socket.get_mut().write_all(&self.capture).await.unwrap();
            assert!(matches!(self.events.recv().await.unwrap(), Some(LinkEvent::Other)));
            let (_, retained) = world.offer_uplink(&[4096; 320]);
            assert_eq!(retained, [4096; 320]);
            self.socket.get_mut().write_all(mute).await.unwrap();
            let frame = MicrophoneRun::recv(&mut run).await.unwrap().unwrap();
            assert_eq!(&*frame.payload, &self.pcm);
            assert!(MicrophoneRun::recv(&mut run).await.unwrap().is_none());
            assert!(matches!(self.events.recv().await.unwrap(), Some(LinkEvent::Mute(true))));
            world.mute();
            self.socket.get_mut().write_all(&self.capture).await.unwrap();
            assert!(matches!(
                self.events.recv().await.unwrap(),
                Some(LinkEvent::MicrophoneDropped(1))
            ));
            assert!(MicrophoneRun::recv(&mut run).await.unwrap().is_none());
            assert_eq!((run.stats().received, run.stats().dropped), (2, 1));
            self.socket.get_mut().write_all(unmute).await.unwrap();
            assert!(matches!(
                self.events.recv().await.unwrap(),
                Some(LinkEvent::Mute(false))
            ));
            assert!(!world.device_view().capture_open);
        }
    }

    pub(super) fn playback_row(&mut self, runtime: &Runtime, world: &Witness, screen: &mut Screen) -> ResultRow {
        let sample = reply_sample();
        let fixture = PlaybackWire::new(&sample);
        runtime.block_on(self.playback(world, screen, &fixture, sample, WARM));
        measure(
            runtime,
            world,
            "playback",
            "Wyoming+SSE+Tokio+playback-probe",
            self.playback(world, screen, &fixture, sample, FRAMES),
        )
    }

    async fn playback(
        &mut self,
        world: &Witness,
        screen: &mut Screen,
        wire: &PlaybackWire,
        sample: ReplyFrame,
        count: usize,
    ) {
        use super::core::RUN;
        world.pacer(PacerEvent::ReplyOpened);
        world.step(echo_core_api::Event::Session(SessionEvent::ReplyStarted {
            session: RUN,
        }));
        let (send, source) = self.link.reply_channel(NonZeroUsize::MIN);
        let play = EchoLink::play_with_timeout(&self.link, REPLY, source, Duration::from_secs(30));
        let sink = async {
            let mut buff = [0; 1200];
            read_bytes(&mut self.socket, &wire.start, &mut buff).await;
            let mut scratch = Box::new(sample);
            let lips_before = screen.batches();
            for _ in 0..count {
                let (frame, returned) = world.due_frame(scratch);
                assert_eq!(frame, sample);
                scratch = returned;
                send.send(frame.to_vec()).await.unwrap();
                read_bytes(&mut self.socket, &wire.chunk, &mut buff).await;
                screen.handed(world).await;
            }
            assert_eq!(screen.batches() - lips_before, count);
            world.pacer(PacerEvent::InputFinished);
            drop(send);
            read_bytes(&mut self.socket, &wire.stop, &mut buff).await;
            self.socket.get_mut().write_all(&wire.played).await.unwrap();
        };
        let (played, ()) = tokio::join!(play, sink);
        assert_eq!(played, Ok(PlayOutcome::Played));
        assert!(matches!(self.events.recv().await.unwrap(), Some(LinkEvent::Played)));
        world.played();
    }
}

struct PlaybackWire {
    start: Vec<u8>,
    chunk: Vec<u8>,
    stop: Vec<u8>,
    played: Vec<u8>,
}

impl PlaybackWire {
    fn new(sample: &ReplyFrame) -> Self {
        let format = json!({"rate":24000,"channels":1,"width":2});
        Self {
            start: wire("audio-start", format.clone(), vec![]),
            chunk: wire("audio-chunk", format, sample.to_vec()),
            stop: wire("audio-stop", Value::Null, vec![]),
            played: wire("played", Value::Null, vec![]),
        }
    }
}

fn wire(kind: &str, data: Value, payload: Vec<u8>) -> Vec<u8> {
    encode(&Event {
        header: Header {
            r#type: kind.into(),
            data: Some(data),
            data_length: 0,
            payload_length: 0,
        },
        data: None,
        payload,
    })
}

async fn read_bytes(socket: &mut BufReader<TcpStream>, expected: &[u8], buf: &mut [u8; 1200]) {
    let actual = &mut buf[..expected.len()];
    socket.read_exact(actual).await.unwrap();
    assert_eq!(actual, expected);
}

fn reply_sample() -> ReplyFrame {
    let mut reply = [0; 960];
    for (index, bytes) in reply.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        *bytes = (if index % 24 < 12 { 9_000_i16 } else { -9_000 }).to_le_bytes();
    }
    reply
}
