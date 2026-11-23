use super::{core::Witness, reply_lips::Lips};
use echo_contracts::secret::Secret;
use echo_core_api::PacerEvent;
use echo_screen_contract::{ScreenFrame, UiState, mouth::MouthBatch};
use echo_screen_server::ScreenServer;
use std::time::Duration;
use tokio::sync::mpsc;

const TOKEN: &str = "synthetic-screen-pair";

pub(super) struct Screen {
    _server: ScreenServer,
    _states: mpsc::Sender<UiState>,
    response: reqwest::Response,
    lips: Lips,
    pending: Vec<u8>,
    batches: usize,
    last_sample: u64,
}

impl Screen {
    pub(super) async fn open() -> Self {
        let (states, receive) = mpsc::channel(1);
        let server = echo_screen_server::serve(([127, 0, 0, 1], 0).into(), Secret::new(TOKEN.into()), receive)
            .await
            .unwrap();
        states.send(UiState::Speaking).await.unwrap();
        server
            .flush(tokio::time::Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{}/events?token={TOKEN}&mouth=1", server.bound_addr))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let lips = Lips::new(Box::new(server.mouth()), Duration::from_millis(40));
        assert_eq!(lips.delay(), Duration::from_millis(40));
        let mut screen = Self {
            _server: server,
            _states: states,
            response,
            lips,
            pending: Vec::with_capacity(512),
            batches: 0,
            last_sample: 0,
        };
        let frame: ScreenFrame = serde_json::from_slice(screen.event(b"data: ").await).unwrap();
        assert_eq!(
            frame,
            ScreenFrame {
                seq: 1,
                state: UiState::Speaking
            }
        );
        assert_eq!(screen.lips.wanted(), Some(true));
        screen
    }

    pub(super) fn batches(&self) -> usize {
        self.batches
    }

    async fn event(&mut self, prefix: &[u8]) -> &[u8] {
        self.pending.clear();
        while !self.pending.ends_with(b"\n\n") {
            let chunk = self.response.chunk().await.unwrap().unwrap();
            self.pending.extend_from_slice(&chunk);
        }
        self.pending
            .strip_prefix(prefix)
            .unwrap()
            .strip_suffix(b"\n\n")
            .unwrap()
    }

    pub(super) async fn handed(&mut self, world: &Witness) {
        let mut commands = world.pacer(PacerEvent::FrameHanded {
            screen_wanted: self.lips.wanted(),
        });
        let batch = commands.take_lips().unwrap();
        assert!(self.lips.send(&batch));
        world.pacer(PacerEvent::LipDelivered { taken: true });
        let observed: MouthBatch = serde_json::from_slice(self.event(b"event: mouth\ndata: ").await).unwrap();
        assert_eq!(observed.sent_ms, batch.sent_ms);
        assert_eq!(observed.samples.len(), 1);
        let sample = &observed.samples[0];
        let expected = batch.samples[0].unwrap();
        assert_eq!((sample.at_ms, sample.open), (expected.at_ms, expected.open));
        assert!(sample.at_ms > self.last_sample);
        assert_eq!(sample.at_ms - observed.sent_ms, 40);
        self.last_sample = sample.at_ms;
        self.batches += 1;
    }
}
