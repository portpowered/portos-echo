//! T1: joint call owner at controlled memory-only signaling and PCM boundaries.
use super::super::call_tests::*;
use super::*;
use echo_media::PeerConfig;
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{mpsc, watch};

struct State {
    states: watch::Sender<PeerState>,
    sent: Mutex<Vec<PcmFrame>>,
    answers: Mutex<Vec<String>>,
    offers: AtomicUsize,
    offer_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    offer_error: Mutex<Option<echo_media::Error>>,
    closes: AtomicUsize,
    resets: AtomicUsize,
    reset_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    observer: Mutex<Option<echo_media::SetupObserver>>,
    applied: tokio::sync::Notify,
    ready: bool,
    answer_error: Option<echo_media::Error>,
}
struct Controlled {
    state: Arc<State>,
    audio: mpsc::Receiver<PcmFrame>,
}
fn peer(ready: bool, answer_error: Option<echo_media::Error>) -> (Controlled, Arc<State>, mpsc::Sender<PcmFrame>) {
    let (states, _) = watch::channel(PeerState::New);
    let state = Arc::new(State {
        states,
        sent: Mutex::default(),
        answers: Mutex::default(),
        offers: AtomicUsize::new(0),
        offer_gate: Mutex::default(),
        offer_error: Mutex::default(),
        closes: AtomicUsize::new(0),
        resets: AtomicUsize::new(0),
        reset_gate: Mutex::default(),
        observer: Mutex::default(),
        applied: tokio::sync::Notify::new(),
        ready,
        answer_error,
    });
    let (tx, audio) = mpsc::channel(4);
    (
        Controlled {
            state: state.clone(),
            audio,
        },
        state,
        tx,
    )
}
impl MediaPeer for Controlled {
    fn create(_: PeerConfig) -> impl std::future::Future<Output = Result<Self, echo_media::Error>> {
        std::future::ready(Err(echo_media::Error::InvalidConfig))
    }
    async fn offer(&self) -> Result<String, echo_media::Error> {
        self.state.offers.fetch_add(1, Ordering::SeqCst);
        self.state.states.send_replace(PeerState::Connecting);
        let gate = self.state.offer_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        if let Some(error) = *self.state.offer_error.lock().unwrap() {
            return Err(error);
        }
        Ok("fixture-offer".into())
    }
    fn apply_answer(&self, sdp: &str) -> impl std::future::Future<Output = Result<(), echo_media::Error>> {
        self.state.answers.lock().unwrap().push(sdp.into());
        if let Some(error) = self.state.answer_error {
            return std::future::ready(Err(error));
        }
        self.state.applied.notify_one();
        if self.state.ready {
            self.state.states.send_replace(PeerState::Connected);
        }
        std::future::ready(Ok(()))
    }
    fn observe_setup(&self, observer: echo_media::SetupObserver) -> Result<(), echo_media::Error> {
        let mut stored = self.state.observer.lock().unwrap();
        if stored.is_some() {
            return Err(echo_media::Error::State);
        }
        *stored = Some(observer);
        Ok(())
    }
    fn states(&self) -> watch::Receiver<PeerState> {
        self.state.states.subscribe()
    }
    fn send(&self, frame: PcmFrame) -> Result<(), echo_media::Error> {
        if *self.state.states.borrow() != PeerState::Connected {
            return Err(echo_media::Error::NotConnected);
        }
        self.state.sent.lock().unwrap().push(frame);
        Ok(())
    }
    async fn recv(&mut self) -> Result<Option<PcmFrame>, echo_media::Error> {
        Ok(self.audio.recv().await)
    }
    async fn reset_receive(&mut self) -> Result<(), echo_media::Error> {
        let gate = self.state.reset_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
        while self.audio.try_recv().is_ok() {}
        self.state.resets.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn dropped_frames(&self) -> DropCounts {
        DropCounts::default()
    }
    fn close(&mut self) -> impl std::future::Future<Output = Result<(), echo_media::Error>> {
        self.state.closes.fetch_add(1, Ordering::SeqCst);
        self.state.states.send_replace(PeerState::Closed);
        std::future::ready(Ok(()))
    }
}

#[tokio::test(start_paused = true)]
async fn media_call_ready_drain_is_nonblocking_ordered_and_distinguishes_eof() {
    let (mut io, mut server) = fixture();
    let (peer, state, tx) = peer(true, None);
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        assert_eq!(call.ready_audio().await, Err(Error::Unavailable));
        call.start(&options(), vec![], deadline()).await.unwrap();
        let start = Instant::now();
        assert_eq!(call.ready_audio().await.unwrap(), None);
        tx.send([1; 320]).await.unwrap();
        tx.send([2; 320]).await.unwrap();
        assert_eq!(call.ready_audio().await.unwrap(), Some([1; 320]));
        assert_eq!(call.ready_audio().await.unwrap(), Some([2; 320]));
        assert_eq!(call.ready_audio().await.unwrap(), None);
        assert_eq!(Instant::now(), start);
        drop(tx);
        assert_eq!(call.ready_audio().await, Err(Error::Unavailable));
        call.close(deadline()).await.unwrap();
        assert_eq!(call.ready_audio().await, Err(Error::Unavailable));
    };
    let remote = async {
        start(&mut server).await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
}

#[path = "voice_media_cases.rs"]
mod voice;

#[path = "voice_session_cases.rs"]
mod session;

#[path = "voice_tools_cases.rs"]
mod tools;

#[path = "voice_notes_cases.rs"]
mod notes;

#[tokio::test(start_paused = true)]
async fn media_call_waits_for_connected_sends_pcm_and_preserves_generation_after_close() {
    let (mut io, mut server) = fixture();
    let (peer, state, tx) = peer(true, None);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.trace = Some(crate::bridge_trace::Trace::open(&path, "api_key").unwrap());
        assert_eq!(call.send([1; 320]), Err(Error::Unavailable));
        assert_eq!(call.start(&options(), vec![], deadline()).await, Ok(()));
        call.send([123; 320]).unwrap();
        tx.send([-456; 320]).await.unwrap();
        assert!(matches!(call.next().await.unwrap().into_parts().1, Some(frame) if frame == [-456;320]));
        assert_eq!(call.dropped_frames(), DropCounts::default());
        assert_eq!(call.close(deadline()).await, Ok(()));
        assert_eq!(call.close(deadline()).await, Ok(()));
        assert_eq!(call.send([1; 320]), Err(Error::Unavailable));
        assert_eq!(call.start(&options(), vec![], deadline()).await, Err(Error::Input));
    };
    let remote = async {
        start(&mut server).await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
    assert_eq!(*state.sent.lock().unwrap(), vec![[123; 320]]);
    assert_eq!(*state.answers.lock().unwrap(), vec!["fixture-answer"]);
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert!(io.begin_request("account/read", &json!({})).is_ok());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "");
}

#[tokio::test(start_paused = true)]
async fn media_call_sdp_without_connectivity_hits_shared_deadline_and_closes_both() {
    let (mut io, mut server) = fixture();
    let (peer, state, _tx) = peer(false, None);
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        let end = deadline();
        assert_eq!(call.start(&options(), vec![], end).await, Err(Error::Deadline));
        assert_eq!(Instant::now(), end);
        assert_eq!(call.send([0; 320]), Err(Error::Unavailable));
    };
    let remote = async {
        start(&mut server).await;
        std::future::pending::<()>().await;
    };
    tokio::select! { () = client => (), () = remote => unreachable!() }
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(echo_codex::codex_rpc::RpcError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn media_call_early_events_flow_during_ice_and_survive_connection() {
    let (mut io, mut server) = fixture();
    let (peer, state, _tx) = peer(false, None);
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), vec![], deadline()).await.unwrap();
        assert!(
            matches!(call.next().await.unwrap().into_parts().0, Some(Event::ReplyDone { text }) if text == "early")
        );
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        notification(
            &mut server,
            "thread/realtime/transcript/done",
            json!({"threadId":"thread-1","role":"assistant","text":"early"}),
        )
        .await;
        // Unknown requests must be serviced even before media connects.
        write(
            &mut server,
            json!({"id":"unknown","method":"unknown/method","params":{}}),
        )
        .await;
        assert_eq!(
            read(&mut server).await,
            json!({"id":"unknown","result":{"contentItems":[],"success":false}})
        );
        state.states.send_replace(PeerState::Connected);
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn media_call_invalid_answer_or_failed_peer_never_becomes_active() {
    for error in [
        echo_media::Error::InvalidSdp,
        echo_media::Error::Transport,
        echo_media::Error::Audio,
    ] {
        let (mut io, mut server) = fixture();
        let (peer, state, _tx) = peer(true, Some(error));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace");
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            call.trace = Some(crate::bridge_trace::Trace::open(&path, "api_key").unwrap());
            assert_eq!(
                call.start(&options(), vec![], deadline()).await,
                Err(media_error(error))
            );
            assert!(matches!(call.next().await, Err(Error::Unavailable)));
        };
        let remote = async {
            start(&mut server).await;
        };
        tokio::join!(client, remote);
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            rows.iter()
                .map(|row| row["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["call.start_failed"]
        );
        assert_eq!(rows[0]["stage"], "answer_received");
        assert_eq!(rows[0]["media_state"], "Connecting");
        assert_eq!(rows[0].as_object().unwrap().len(), 5);
    }
}

#[tokio::test(start_paused = true)]
async fn media_call_stop_failure_still_releases_peer_and_blocks_further_audio() {
    let (mut io, mut server) = fixture();
    let (peer, state, _tx) = peer(true, None);
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), vec![], deadline()).await.unwrap();
        assert_eq!(call.close(deadline()).await, Err(Error::Deadline));
        assert_eq!(call.send([7; 320]), Err(Error::Unavailable));
    };
    let remote = async {
        start(&mut server).await;
        assert_eq!(read(&mut server).await["method"], "thread/realtime/stop");
        std::future::pending::<()>().await;
    };
    tokio::select! { () = client => (), () = remote => unreachable!() }
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn media_call_transcript_end_does_not_consume_or_close_accepted_audio() {
    let (mut io, mut server) = fixture();
    let (peer, state, tx) = peer(true, None);
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        call.start(&options(), vec![], deadline()).await.unwrap();
        assert!(matches!(call.next().await.unwrap().into_parts().0, Some(Event::ReplyDone { text }) if text == "done"));
        tx.send([777; 320]).await.unwrap();
        assert!(matches!(call.next().await.unwrap().into_parts().1, Some(frame) if frame == [777;320]));
        assert_eq!(state.closes.load(Ordering::SeqCst), 0);
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        notification(
            &mut server,
            "thread/realtime/transcript/done",
            json!({"threadId":"thread-1","role":"assistant","text":"done"}),
        )
        .await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}

#[tokio::test(start_paused = true)]
async fn media_call_cancelling_ice_then_cleanup_closes_generation_and_media() {
    let (mut io, mut server) = fixture();
    let (peer, state, _tx) = peer(false, None);
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                call.start(&options(), vec![], deadline())
            )
            .await
            .is_err()
        );
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        std::future::pending::<()>().await;
    };
    tokio::select! { () = client => (), () = remote => unreachable!() }
    assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    assert_eq!(
        io.begin_request("account/read", &json!({})),
        Err(echo_codex::codex_rpc::RpcError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn media_call_rejects_early_event_overflow_or_close_before_ice() {
    for case in ["overflow", "closed", "failed"] {
        let (mut io, mut server) = fixture();
        let (peer, state, _tx) = peer(false, None);
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            assert_eq!(
                call.start(&options(), vec![], deadline()).await,
                Err(Error::Unavailable)
            );
            assert_eq!(call.send([1; 320]), Err(Error::Unavailable));
        };
        let remote = async {
            start(&mut server).await;
            match case {
                "overflow" => {
                    for _ in 0..33 {
                        notification(
                            &mut server,
                            "thread/realtime/transcript/delta",
                            json!({"threadId":"thread-1","role":"assistant","delta":"early"}),
                        )
                        .await;
                    }
                }
                "closed" => notification(&mut server, "thread/realtime/closed", json!({"threadId":"thread-1"})).await,
                _ => {
                    state.states.send_replace(PeerState::Failed);
                }
            }
            std::future::pending::<()>().await;
        };
        tokio::select! { () = client => (), () = remote => unreachable!() }
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn media_call_media_eof_or_process_error_closes_both_without_reply_success() {
    for process_error in [false, true] {
        let (mut io, mut server) = fixture();
        let (peer, state, tx) = peer(true, None);
        let client = async {
            let mut call = MediaCall::new(&mut io, peer);
            call.start(&options(), vec![], deadline()).await.unwrap();
            if !process_error {
                drop(tx);
            }
            assert!(matches!(call.next().await, Err(Error::Unavailable)));
            assert_eq!(call.send([1; 320]), Err(Error::Unavailable));
        };
        let remote = async {
            start(&mut server).await;
            if process_error {
                notification(
                    &mut server,
                    "thread/realtime/error",
                    json!({"threadId":"thread-1","message":"private"}),
                )
                .await;
            }
            std::future::pending::<()>().await;
        };
        tokio::select! { () = client => (), () = remote => unreachable!() }
        assert_eq!(state.closes.load(Ordering::SeqCst), 1);
        assert_eq!(
            io.begin_request("account/read", &json!({})),
            Err(echo_codex::codex_rpc::RpcError::Disconnected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn media_call_forwards_tool_response_under_original_id_while_media_is_live() {
    let (mut io, mut server) = fixture();
    let (peer, _state, _tx) = peer(true, None);
    let result = ToolResult {
        content_items: vec![],
        success: false,
    };
    let client = async {
        let mut call = MediaCall::new(&mut io, peer);
        assert_eq!(
            call.answer_tool(&RequestId::Number(42), &result),
            Err(Error::Unavailable)
        );
        call.start(&options(), vec![], deadline()).await.unwrap();
        let Some(Event::Tool { id, call: tool }) = call.next().await.unwrap().into_parts().0 else {
            unreachable!()
        };
        assert_eq!(id, RequestId::Number(42));
        assert_eq!(tool.tool, "tool_0");
        call.answer_tool(&id, &result).unwrap();
        assert_eq!(call.answer_tool(&id, &result), Err(Error::Unavailable));
        assert!(matches!(
            call.next().await.unwrap().into_parts().0,
            Some(Event::ReplyDone { .. })
        ));
        call.close(deadline()).await.unwrap();
    };
    let remote = async {
        start(&mut server).await;
        write(
            &mut server,
            json!({"id":42,"method":"item/tool/call","params":{
                "threadId":"thread-1","turnId":"turn-1","callId":"call-1","tool":"tool_0","arguments":{}
            }}),
        )
        .await;
        assert_eq!(
            read(&mut server).await,
            json!({"id":42,"result":{"contentItems":[],"success":false}})
        );
        notification(
            &mut server,
            "thread/realtime/transcript/done",
            json!({"threadId":"thread-1","role":"assistant","text":"done"}),
        )
        .await;
        stop(&mut server).await;
    };
    tokio::join!(client, remote);
}
