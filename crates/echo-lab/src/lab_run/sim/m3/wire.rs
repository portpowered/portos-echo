//! Transparent native transport witness; no fabricated wake, mute or Played.
use echo_contracts::wyoming::Event;
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    pub inputs: usize,
    pub flushes: usize,
    pub tail_bytes: usize,
    pub live_tail_frames: u64,
    pub overlap_base: Option<u64>,
    pub receive_resets: usize,
    pub cut_frames: Option<u64>,
    pub runs: usize,
    pub capture_frames: u64,
    pub old_sink_pcm: Option<u64>,
    pub flush_observation: Option<serde_json::Value>,
    pub ends: usize,
    pub bytes: [usize; 2],
    pub started: usize,
    pub stops: usize,
    pub played: Vec<Instant>,
    pub cancel: Option<Instant>,
    pub stimulus: Option<Instant>,
    pub cut_stop: Option<Instant>,
    pub cut_order: Option<&'static str>,
    pub last_old_reply: Option<Instant>,
    pub after_window: usize,
    pub after_stop: usize,
    pub muted: bool,
    pub mute_edges: usize,
    pub failure: Option<String>,
}

impl Ledger {
    /// Scenario observations come from native protocol facts. An ordinary stop
    /// or a provider signal alone never establishes an acknowledged cut.
    pub fn observations(&self) -> Vec<serde_json::Value> {
        let mut records = Vec::new();
        let mut record = |event, outcome| {
            let mut value = serde_json::json!({"event":event, "run_id":"run-1", "source":"wyoming-wire"});
            if let Some(outcome) = outcome {
                value["outcome"] = serde_json::json!(outcome);
            }
            records.push(value);
        };
        if self.bytes[0] > 0 {
            record("reply.started", None::<&str>);
        }
        if self.cancel.is_some() && self.continues == 1 && self.stops > 0 {
            record("reply.cut", None);
            if !self.played.is_empty() {
                record("reply.played", Some("cut"));
            }
        }
        records
    }

    fn reply(&mut self, event: &Event) -> Result<(), String> {
        match event.header.r#type.as_str() {
            "audio-start" => {
                if self.started >= 2 || self.started != self.stops + self.flushes {
                    return Err("M3 wire: unexpected reply start".into());
                }
                self.started += 1;
            }
            "hassmic-flush" => {
                self.cut_order = Some(if !self.played.is_empty() {
                    "after_played"
                } else if self.stops == 1 {
                    "after_stop_before_played"
                } else {
                    "before_stop"
                });
                if self.started != 1 || !self.played.is_empty() || self.flushes != 0 {
                    return Err("M3 wire: flush must cut the first reply exactly once".into());

                }
                self.flushes += 1;
                self.cut_stop = Some(Instant::now());

            }
            "audio-stop" => self.stops += 1,
            "hassmic-end" => self.ends += 1,

            "audio-chunk" => {
                let phase = self.started.checked_sub(1).ok_or("M3 wire: PCM before start")?;
                if phase >= 2 || event.payload.len() != 960 {
                    return Err("M3 wire: expected complete 20 ms reply PCM".into());
                }
                self.bytes[phase] += event.payload.len();
                if phase == 0 {
                    self.last_old_reply = Some(Instant::now());
                }
                if self.stops + self.flushes == self.started {

                    self.after_stop += event.payload.len();
                }
                if phase == 0 && self.cancel.is_some_and(|at| at.elapsed() >= Duration::from_millis(200)) {
                    self.after_window += event.payload.len();
                }
            }
            _ => (),
        }
        Ok(())
    }
}

pub struct Relay {
    pub address: SocketAddr,
    pub observed: watch::Sender<Ledger>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), String>>,
}

impl Relay {
    pub async fn bind(address: SocketAddr, fixture: &[u8]) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "M3 wire: bind failed")?;
        let local = listener.local_addr().map_err(|_| "M3 wire: address failed")?;
        let (observed, _) = watch::channel(Ledger::default());
        let sender = observed.clone();
        let marker = signs(fixture);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                result = serve(listener, address, marker, &sender) => result,
                _ = stopped => Ok(()),
            };
            if let Err(error) = &result {
                sender.send_modify(|state| state.failure = Some(error.clone()));
            }
            result
        });
        Ok(Self {
            address: local,
            observed,
            stop: Some(stop),
            task,
        })
    }

    pub async fn wait(&self, property: impl Fn(&Ledger) -> bool) -> Result<Ledger, String> {
        let mut receiver = self.observed.subscribe();
        loop {
            let state = receiver.borrow().clone();
            if let Some(error) = state.failure {
                return Err(error);
            }
            if state.after_window != 0 || state.after_stop != 0 {
                return Err(format!(
                    "M3 old reply: expected bytes after cancel window=0/stop=0, observed {}/{}",
                    state.after_window, state.after_stop
                ));
            }
            if property(&state) {
                return Ok(state);
            }
            receiver.changed().await.map_err(|_| "M3 wire: observer ended")?;
        }
    }

    pub async fn close(mut self) -> Result<(), String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.map_err(|_| "cleanup: M3 relay task failed")?
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    listener: TcpListener,
    address: SocketAddr,
    marker: Vec<u8>,
    observed: &watch::Sender<Ledger>,
) -> Result<(), String> {
    let (bridge, _) = listener.accept().await.map_err(|_| "M3 wire: accept failed")?;
    drop(listener);
    let host = TcpStream::connect(address)
        .await
        .map_err(|_| "M3 wire: host connect failed")?;
    let (mut bridge_in, mut bridge_out) = bridge.into_split();
    let (mut host_in, mut host_out) = host.into_split();
    let mut tail = Vec::new();
    tokio::select! {
        result = async {
            while let Some((bytes, event)) = super::super::m4::wire::frame(&mut host_in).await? {
                bridge_out.write_all(&bytes).await.map_err(|_| "M3 wire: capture write failed")?;
                observed.send_modify(|state| capture(state, &event, &marker, &mut tail));
            }
            Ok(())
        } => result,
        result = async {
            while let Some((bytes, event)) = super::super::m4::wire::frame(&mut bridge_in).await? {
                host_out.write_all(&bytes).await.map_err(|_| "M3 wire: reply write failed")?;
                let mut result = Ok(());
                observed.send_modify(|state| result = state.reply(&event));
                result?;
            }
            Ok(())
        } => result,
    }
}

fn capture(state: &mut Ledger, event: &Event, marker: &[u8], tail: &mut Vec<u8>) {
    match event.header.r#type.as_str() {
        "run-pipeline" => state.runs += 1,
        "played" => state.played.push(Instant::now()),
        "hassmic-mute" => {
            if let Some(muted) = event
                .data
                .as_ref()
                .and_then(|data| data.get("muted"))
                .and_then(serde_json::Value::as_bool)
            {
                state.muted = muted;
                state.mute_edges += 1;
            }
        }
        "audio-chunk" => {
            state.capture_frames += 1;
            if state.played.len() == 1 {
                state.tail_bytes += event.payload.len();
            }
            tail.extend(signs(&event.payload));
            if tail.windows(marker.len()).any(|part| part == marker) {
                state.inputs += 1;
                tail.clear();
            } else {
                tail.drain(..tail.len().saturating_sub(marker.len() - 1));
            }
        }
        _ => (),
    }
}

fn signs(pcm: &[u8]) -> Vec<u8> {
    pcm.as_chunks::<2>()
        .0
        .iter()
        .map(|part| {
            let value = i16::from_le_bytes([part[0], part[1]]);
            u8::from(value > 0) + 2 * u8::from(value < 0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_contracts::wyoming::Header;

    fn event(name: &str, bytes: usize) -> Event {
        Event {
            header: Header {
                r#type: name.into(),
                data: None,
                data_length: 0,
                payload_length: bytes,
            },
            data: None,
            payload: vec![0; bytes],
        }
    }

    #[tokio::test(start_paused = true)]
    async fn exact_inflight_window_and_stopped_reply_reject_additional_pcm() {
        let mut state = Ledger {
            cancel: Some(Instant::now()),
            ..Ledger::default()
        };
        state.reply(&event("audio-start", 0)).unwrap();
        tokio::time::advance(Duration::from_micros(199_999)).await;
        state.reply(&event("audio-chunk", 960)).unwrap();
        assert_eq!((state.bytes, state.after_window), ([960, 0], 0));
        tokio::time::advance(Duration::from_micros(1)).await;
        state.reply(&event("audio-chunk", 960)).unwrap();
        assert_eq!(state.after_window, 960);
        state.reply(&event("audio-stop", 0)).unwrap();
        state.reply(&event("audio-chunk", 960)).unwrap();
        assert_eq!(state.after_stop, 960);
        state.reply(&event("audio-start", 0)).unwrap();
        state.reply(&event("audio-chunk", 960)).unwrap();
        assert_eq!(state.bytes, [2880, 960]);
        assert_eq!((state.after_stop, state.after_window), (960, 1920));
    }

    #[test]
    fn cut_after_stop_before_played_is_once_and_keeps_replacement_phase() {
        let mut state = Ledger::default();
        state.reply(&event("audio-start", 0)).unwrap();
        state.reply(&event("audio-chunk", 960)).unwrap();
        state.reply(&event("audio-stop", 0)).unwrap();
        assert_eq!(state.stops, 1);
        assert_eq!(state.played.len(), 0);
        state.reply(&event("hassmic-continue", 0)).unwrap();
        assert!(state.reply(&event("hassmic-continue", 0)).is_err());
        assert!(state.reply(&event("audio-stop", 0)).is_err());
        assert!(state.cut_stop.is_some());
        assert_eq!(state.cut_order, Some("after_stop_before_played"));
        assert_eq!((state.stops, state.continues), (1, 1));
        state.played.push(Instant::now());
        state.reply(&event("audio-start", 0)).unwrap();
        state.reply(&event("audio-chunk", 960)).unwrap();
        state.reply(&event("audio-stop", 0)).unwrap();
        assert_eq!((state.bytes, state.stops, state.after_stop), ([960, 960], 2, 0));
        let mut completed = Ledger {
            started: 1,
            stops: 1,
            played: vec![Instant::now()],
            ..Ledger::default()
        };
        assert!(completed.reply(&event("hassmic-continue", 0)).is_err());
        assert_eq!(completed.cut_order, Some("after_played"));
    }

    #[test]
    fn malformed_frames_wrong_phase_and_duplicate_starts_fail() {
        let mut state = Ledger::default();
        assert!(state.reply(&event("audio-chunk", 960)).is_err());
        state.reply(&event("audio-start", 0)).unwrap();
        assert!(state.reply(&event("audio-start", 0)).is_err());
        for bytes in [0, 1, 959, 961, 1920] {
            assert!(state.reply(&event("audio-chunk", bytes)).is_err());
        }
    }

    #[test]
    fn cut_observations_require_native_continuation_stop_and_played() {
        let mut state = Ledger {
            cancel: Some(Instant::now()),
            bytes: [960, 0],
            stops: 1,
            played: vec![Instant::now()],
            ..Ledger::default()
        };
        assert_eq!(state.observations().len(), 1);
        state.continues = 1;
        let records = state.observations();
        assert_eq!(records.len(), 3);
        assert_eq!(records[1]["event"], "reply.cut");
        assert_eq!(records[2]["event"], "reply.played");
        assert_eq!(records[2]["outcome"], "cut");
        state.played.clear();
        assert_eq!(state.observations().len(), 2);
        state.stops = 0;
        assert_eq!(state.observations().len(), 1);
    }
}
