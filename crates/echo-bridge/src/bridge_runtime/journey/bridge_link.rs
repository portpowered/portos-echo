//! T1: the production journey, driven by in-memory device and Live ports only.
use super::*;
use crate::{
    bridge_health::Health,
    bridge_input,
    bridge_runtime::{
        bridge_delegation::Host, bridge_kill::Kill, bridge_lifecycle::select_wake, bridge_screen::Screen,
    },
};
use echo_contracts::ids::DelegationId;
use echo_link::{
    LinkConnector, LinkEvents, MicrophoneRun,
    fake::{Command, FakeConnector, Observer},
};
use echo_live::AudioFrame;
use std::io::Write;
use std::{collections::VecDeque, future::Future, pin::Pin, task::Poll};

#[derive(Default)]
struct Live {
    sent: Vec<AudioFrame>,
    audio: VecDeque<AudioFrame>,
    events: VecDeque<LiveEvent>,
    echo: bool,
    reject_at: Option<usize>,
    cancel_at: Option<Instant>,
}
impl LivePort for Live {
    fn send_audio(
        &mut self,
        frame: AudioFrame,
        _: std::time::Instant,
    ) -> impl Future<Output = Result<(), CallError>> + Send {
        if self.reject_at == Some(self.sent.len()) {
            return std::future::ready(Err(CallError::Unavailable));
        }
        if self.echo {
            self.audio.push_back(frame.clone());
        }
        self.sent.push(frame);
        if self.echo && self.sent.len() == 32 {
            self.events
                .push_back(LiveEvent::Other(json!({"type":"fake.audio.frames","count":32})));
        }
        std::future::ready(Ok(()))
    }
    async fn receive_event(&mut self, deadline: std::time::Instant) -> Result<LiveEvent, CallError> {
        if let Some(event) = self.events.pop_front() {
            return Ok(event);
        }
        let deadline = Instant::from_std(deadline);
        tokio::time::sleep_until(self.cancel_at.map_or(deadline, |at| at.min(deadline))).await;
        if self.cancel_at.is_some_and(|at| Instant::now() >= at) {
            self.cancel_at = None;
            let event = LiveEvent::Other(json!({"type":"response.done","response":{"status":"cancelled"}}));
            self.events.push_back(event.clone());
            return Ok(event);
        }
        Err(CallError::Deadline)
    }
    fn append_commentary(&mut self, _: DelegationId, _: String) -> impl Future<Output = Result<(), CallError>> + Send {
        std::future::ready(Err(CallError::Unavailable))
    }
    fn pop_audio(&mut self) -> Option<AudioFrame> {
        self.audio.pop_front()
    }
    fn close(&mut self) {}
}

async fn pending<F: Future>(mut future: Pin<&mut F>) {
    // Poll cooperative yields to quiescence without advancing the fake clock.
    for _ in 0..128 {
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
}
fn frames(observer: &Observer) -> Vec<Vec<u8>> {
    observer
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            Command::Frame(frame) => Some(frame),
            _ => None,
        })
        .collect()
}
fn trace(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn settings() -> bridge_input::Settings {
    bridge_input::parse(
        &[],
        [
            Some("127.0.0.1:1".into()),
            Some("http://127.0.0.1:2".into()),
            Some("unused".into()),
        ],
    )
    .unwrap()
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn bridge_link_wake_mic_paced_echo_acknowledgment_and_fresh_run() {
    let observer = Observer::default();
    let mut link = FakeConnector::new(observer.clone()).connect().await.unwrap();
    let mut events = link.events().unwrap();
    let settings = settings();
    let screen = Screen::bind(None).await.unwrap();
    let health = Health::offline();
    let kill = Kill {
        settings: &settings,
        health: &health,
        screen: &screen,
    };
    let mut host = Host::new(&settings, std::sync::Arc::new(|| 0)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    for phrase in ["Computer", "a fresh phrase"] {
        observer.wake(
            phrase,
            (0..10)
                .map(|_| MicFrame {
                    format: bridge_audio::format(16_000),
                    payload: vec![0; 2048],
                })
                .collect(),
        );
        let run = select_wake(events.recv().await.unwrap()).unwrap().unwrap();
        assert_eq!(run.wake_phrase(), phrase);
        assert_eq!(run.stats().received, 10);
        let path = dir.path().join(phrase);
        let mut trace_writer = Trace::open(&path, "api_key").unwrap();
        let mut live = Live {
            echo: true,
            ..Live::default()
        };
        let base = frames(&observer).len();
        let mut journey = Box::pin(super::run(
            &link,
            &mut events,
            run,
            &mut live,
            &mut trace_writer,
            "run-1",
            (true, 200, 0, &screen, &mut host, &kill),
        ));
        pending(journey.as_mut()).await;
        assert_eq!(frames(&observer).len() - base, 10);
        for count in 11..=32 {
            tokio::time::advance(Duration::from_millis(20)).await;
            pending(journey.as_mut()).await;
            assert_eq!(frames(&observer).len() - base, count);
        }
        assert_eq!(observer.commands().last(), Some(&Command::Stop));
        observer.finish(Ok(()));
        assert_eq!(journey.await.unwrap(), (32, true));
        assert_happy(&live, &observer, base, &path);
    }
    link.close().await.unwrap();
}

fn assert_happy(live: &Live, observer: &Observer, base: usize, path: &std::path::Path) {
    assert_eq!(live.sent.len(), 32);
    for (index, packet) in live.sent.iter().enumerate() {
        assert_eq!(usize::from(packet.sequence), index);
        assert_eq!(packet.timestamp, u32::try_from(index).unwrap() * 960);
    }
    assert_eq!(frames(observer)[base..].concat(), vec![0; 30_720]);
    assert_eq!(
        trace(path),
        vec![
            row("mic.frames", 0, json!({"phase":"first","count":2})),
            row("reply.started", 0, json!({})),
            row("reply.played", 440, json!({"outcome":"played"})),
            row("wyoming.mic_frames", 440, json!({"count":10})),
            row(
                "mic.completed",
                440,
                json!({"count":32,"received_chunks":10,"dropped_chunks":0,
            "input_samples":10240,"tail_samples":0})
            ),
            row("mic.frames", 440, json!({"phase":"completed","count":32})),
            row("wyoming.run_ended", 440, json!({})),
        ]
    );
}
fn row(event: &str, elapsed: u64, fields: serde_json::Value) -> serde_json::Value {
    let mut value = json!({"event":event,"elapsed_ms":elapsed,"provider_auth_mode":"api_key","run_id":"run-1"});
    let serde_json::Value::Object(fields) = fields else {
        unreachable!()
    };
    value.as_object_mut().unwrap().extend(fields);
    value
}

#[path = "bridge_link_cases.rs"]
mod cases;

#[tokio::test(start_paused = true)]
async fn bridge_link_guard_rejects_microphone_even_when_a_neutral_source_yields_while_muted() {
    let screen = Screen::bind(None).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut trace = Trace::open(&dir.path().join("trace"), "api_key").unwrap();
    screen
        .observe(&echo_link::LinkEvent::<()>::Mute(true), &mut trace)
        .unwrap();
    let mut source = Source::new().unwrap();
    let mut live = Live::default();
    for _ in 0..2 {
        source
            .microphone(
                MicFrame {
                    format: bridge_audio::format(16_000),
                    payload: vec![0; 2048],
                },
                &mut live,
                false,
                &screen,
            )
            .await
            .unwrap();
    }
    assert_eq!(source.chunks, 2);
    assert_eq!(source.sent, 0);
    assert!(source.mic.is_none());
    assert_eq!(live.sent.len(), 0);
    screen
        .observe(&echo_link::LinkEvent::<()>::Mute(false), &mut trace)
        .unwrap();
    let row: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("trace")).unwrap()).unwrap();
    assert_eq!(row["received_frames"], 2);
    assert_eq!(row["forwarded_frames"], 0);
}

#[tokio::test(start_paused = true)]
async fn bridge_link_partial_batch_failure_retains_only_accepted_sends() {
    let screen = Screen::bind(None).await.unwrap();
    let mut source = Source::new().unwrap();
    let mut live = Live {
        reject_at: Some(1),
        ..Live::default()
    };
    assert_eq!(
        source
            .microphone(
                MicFrame {
                    format: bridge_audio::format(16_000),
                    payload: vec![0; 2048],
                },
                &mut live,
                false,
                &screen
            )
            .await,
        Err(Error::Unavailable)
    );
    assert_eq!(source.sent, 1);
    assert_eq!(live.sent.len(), 1);
    assert!(!source.finished);
}

#[tokio::test(start_paused = true)]
async fn bridge_link_flush_short_tail_and_repeated_finish_count_actual_sends() {
    let screen = Screen::bind(None).await.unwrap();
    for samples in [0, 1, 320, 640, 1024] {
        let mut source = Source::new().unwrap();
        let mut live = Live::default();
        source
            .microphone(
                MicFrame {
                    format: bridge_audio::format(16_000),
                    payload: vec![0; samples * 2],
                },
                &mut live,
                false,
                &screen,
            )
            .await
            .unwrap();
        let before = source.sent;
        source.finish_mic(&mut live, &screen).await.unwrap();
        let expected = (samples * 3 / 960) as u64;
        assert_eq!(source.sent, expected);
        assert_eq!(source.sent, live.sent.len() as u64);
        assert_eq!(source.mic.as_ref().unwrap().tail_samples(), samples * 3 % 960);
        source.finish_mic(&mut live, &screen).await.unwrap();
        assert_eq!(source.sent, expected);
        assert_eq!(source.sent, live.sent.len() as u64);
        writeln!(
            std::io::stdout(),
            "samples={samples} before_flush={before} accepted={} tail={}",
            source.sent,
            samples * 3 % 960
        )
        .unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn bridge_link_flush_failure_excludes_rejected_packet_and_bad_pcm() {
    let screen = Screen::bind(None).await.unwrap();
    let mut source = Source::new().unwrap();
    let mut live = Live::default();
    source
        .microphone(
            MicFrame {
                format: bridge_audio::format(16_000),
                payload: vec![0; 2048],
            },
            &mut live,
            false,
            &screen,
        )
        .await
        .unwrap();
    assert_eq!(source.sent, 2);
    live.reject_at = Some(2);
    assert_eq!(source.finish_mic(&mut live, &screen).await, Err(Error::Unavailable));
    assert_eq!(source.sent, 2);
    assert_eq!(live.sent.len(), 2);
    assert!(!source.finished);
    assert_eq!(
        source
            .microphone(
                MicFrame {
                    format: bridge_audio::format(16_000),
                    payload: vec![0; 3],
                },
                &mut live,
                false,
                &screen
            )
            .await,
        Err(Error::Audio)
    );
    assert_eq!(source.sent, 2);
}

#[tokio::test(start_paused = true)]
async fn bridge_link_mute_between_batches_resumes_with_only_accepted_total() {
    let screen = Screen::bind(None).await.unwrap();
    let mut source = Source::new().unwrap();
    let mut live = Live::default();
    let directory = tempfile::tempdir().unwrap();
    let mut writer = Trace::open(&directory.path().join("trace"), "api_key").unwrap();
    for muted in [false, true, false] {
        screen
            .observe(&echo_link::LinkEvent::<()>::Mute(muted), &mut writer)
            .unwrap();
        let before = live.sent.len();
        source
            .microphone(
                MicFrame {
                    format: bridge_audio::format(16_000),
                    payload: vec![0; 2048],
                },
                &mut live,
                false,
                &screen,
            )
            .await
            .unwrap();
        assert_eq!(source.sent, live.sent.len() as u64);
        if muted {
            assert_eq!(live.sent.len(), before);
        }
    }
    source.finish_mic(&mut live, &screen).await.unwrap();
    assert_eq!(source.sent, 6);
    assert_eq!(live.sent.len(), 6);
}

#[tokio::test(start_paused = true)]
async fn bridge_link_invalid_or_rejected_first_input_keeps_zero() {
    let screen = Screen::bind(None).await.unwrap();
    for bytes in [3, 2048] {
        let mut source = Source::new().unwrap();
        let mut live = Live {
            reject_at: Some(0),
            ..Live::default()
        };
        assert_eq!(
            source
                .microphone(
                    MicFrame {
                        format: bridge_audio::format(16_000),
                        payload: vec![0; bytes]
                    },
                    &mut live,
                    false,
                    &screen
                )
                .await,
            Err(if bytes == 3 { Error::Audio } else { Error::Unavailable })
        );
        assert_eq!(source.sent, 0);
        assert_eq!(live.sent.len(), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn lab_microphone_post_drop_samples() {
    let screen = Screen::bind(None).await.unwrap();
    for (lab, short, samples, packets, tail) in [
        (true, true, 10240, 32, 0),
        (true, false, 10240, 32, 0),
        (false, true, 10752, 33, 576),
    ] {
        let mut source = Source::new().unwrap();
        let mut live = Live::default();
        for index in 0..11 {
            let frame = MicFrame {
                format: bridge_audio::format(16_000),
                payload: vec![0; if short && index == 7 { 1024 } else { 2048 }],
            };
            source.microphone(frame, &mut live, lab, &screen).await.unwrap();
        }
        assert_eq!(source.finished, lab);
        source.finish_mic(&mut live, &screen).await.unwrap();
        let mic = source.mic.as_ref().unwrap();
        assert_eq!(
            (source.sent, mic.input_samples, mic.tail_samples()),
            (packets, samples, tail)
        );
        assert_eq!(live.sent.len() as u64, packets);
        for (index, packet) in live.sent.iter().enumerate() {
            assert_eq!(usize::from(packet.sequence), index);
            assert_eq!(packet.timestamp, u32::try_from(index).unwrap() * 960);
        }
        source.finish_mic(&mut live, &screen).await.unwrap();
        assert_eq!(source.sent, packets);
        assert_eq!(live.sent.len() as u64, packets);
    }
}

#[tokio::test(start_paused = true)]
async fn lab_microphone_validates_entire_crossing_frame_before_clipping() {
    let screen = Screen::bind(None).await.unwrap();
    for (bytes, rate) in [(2049, 16_000), (8194, 16_000), (2048, 24_000)] {
        let mut source = Source::new().unwrap();
        let mut live = Live::default();
        for _ in 0..9 {
            source
                .microphone(
                    MicFrame {
                        format: bridge_audio::format(16_000),
                        payload: vec![0; 2048],
                    },
                    &mut live,
                    true,
                    &screen,
                )
                .await
                .unwrap();
        }
        let before = source.sent;
        assert_eq!(
            source
                .microphone(
                    MicFrame {
                        format: bridge_audio::format(rate),
                        payload: vec![0; bytes],
                    },
                    &mut live,
                    true,
                    &screen
                )
                .await,
            Err(Error::Audio)
        );
        assert_eq!(source.mic.as_ref().unwrap().input_samples, 9216);
        assert_eq!(source.sent, before);
        assert_eq!(live.sent.len() as u64, before);
        assert!(!source.finished);
    }
}
