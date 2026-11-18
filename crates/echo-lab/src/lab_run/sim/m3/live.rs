//! Lane-local controlled provider using the existing public WebRTC boundary.
use super::super::{m2::codex::tone, process::Process};
use super::wire::Ledger;
use echo_live::LivePeer;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Default)]
pub struct Received {
    pub frames: u64,
    pub phase: u8,
    pub active: bool,
    pub first: bool,
    pub second: bool,
    pub transcript: Option<String>,
    pub failure: Option<String>,
}

enum Request {
    Snapshot(oneshot::Sender<Received>),
    Quiesce(oneshot::Sender<()>),
}

impl Request {
    fn respond(self, observed: &watch::Sender<Received>) -> bool {
        match self {
            Self::Snapshot(reply) => {
                let _ = reply.send(observed.borrow().clone());
                false
            }
            Self::Quiesce(reply) => {
                let _ = reply.send(());
                true
            }
        }
    }
}

pub struct Live {
    pub origin: String,
    pub observed: watch::Sender<Received>,
    snapshots: mpsc::Sender<Request>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), String>>,
    closing: Arc<AtomicBool>,
}
impl Live {
    pub async fn bind(python: &str, wire: watch::Sender<Ledger>) -> Result<Self, String> {
        let mut command = tokio::process::Command::new(python);
        command
            .arg("-u")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/m3/provider.py"));
        let mut process = Process::start(&mut command)?;
        let ready = process.expect("provider.ready").await?;
        let origin = ready["origin"]
            .as_str()
            .ok_or("M3: provider origin missing")?
            .to_owned();
        let (observed, _) = watch::channel(Received::default());
        let sender = observed.clone();
        let (stop, stopped) = oneshot::channel();
        let (snapshots, requests) = mpsc::channel(1);
        let closing = Arc::new(AtomicBool::new(false));
        let owner_closing = closing.clone();
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                result = session(&mut process, wire, &sender, requests) => result,
                _ = stopped => Ok(()),
            };
            let closed = process.close().await;
            let result = result.and(closed);
            sender.send_modify(|state| state.active = false);
            if let Err(error) = &result {
                if owner_closing.load(Ordering::Acquire) {
                    return Ok(());
                }
                sender.send_modify(|state| state.failure = Some(error.clone()));
            }
            result
        });
        Ok(Self {
            origin,
            observed,
            snapshots,
            stop: Some(stop),
            task,
            closing,
        })
    }
    pub async fn close(mut self) -> Result<(), String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.map_err(|_| "cleanup: M3 Live task failed")?
    }
    pub fn prepare_close(&self) {
        self.closing.store(true, Ordering::Release);
    }
    pub async fn quiesce(&self) -> Result<(), String> {
        let (reply, received) = oneshot::channel();
        self.snapshots
            .send(Request::Quiesce(reply))
            .await
            .map_err(|_| "M3: media owner ended")?;
        tokio::time::timeout(Duration::from_secs(2), received)
            .await
            .map_err(|_| "M3: media quiesce deadline")?
            .map_err(|_| "M3: media did not quiesce".into())
    }
    pub async fn snapshot(&self) -> Result<u64, String> {
        let (reply, received) = oneshot::channel();
        self.snapshots
            .send(Request::Snapshot(reply))
            .await
            .map_err(|_| "M3 mute: Live observer ended")?;
        let state = tokio::time::timeout(Duration::from_secs(2), received)
            .await
            .map_err(|_| "M3 mute: Live snapshot deadline")?
            .map_err(|_| "M3 mute: Live observer ended")?;
        if self.task.is_finished() {
            return Err("M3 mute: Live observer ended".into());
        }
        state.snapshot()
    }
}
impl Received {
    fn snapshot(&self) -> Result<u64, String> {
        if !self.active || self.failure.is_some() {
            return Err("M3 mute: Live observer ended or failed".into());
        }
        Ok(self.frames)
    }
}
impl Drop for Live {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn session(
    process: &mut Process,
    wire: watch::Sender<Ledger>,
    observed: &watch::Sender<Received>,
    requests: mpsc::Receiver<Request>,
) -> Result<(), String> {
    let offer = process.expect("provider.offer").await?;
    let mut peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .map_err(|_| "M3 Live: peer bind failed")?;
    let result = async {
        let answer = peer
            .answer(offer["offer"].as_str().ok_or("M3 Live: missing offer")?)
            .await
            .map_err(|_| "M3 Live: invalid offer")?;
        process.send(&json!({"answer":answer})).await?;
        process.expect("provider.sideband").await?;
        replay(&mut peer, process, wire, observed, requests).await
    }
    .await;
    peer.close();
    result
}

async fn replay(
    peer: &mut LivePeer,
    process: &mut Process,
    wire: watch::Sender<Ledger>,
    observed: &watch::Sender<Received>,
    mut requests: mpsc::Receiver<Request>,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut frames = 0;
    let mut phase = 0;
    let mut quiesced = false;
    loop {
        if quiesced {
            let request = requests.recv().await.ok_or("M3: media control ended")?;
            request.respond(observed);
            continue;
        }
        drive(peer, deadline).await?;
        while peer.pop_text().is_some() {}
        while peer.pop_audio().is_some() {
            frames += 1;
        }
        observed.send_modify(|state| {
            state.frames = frames;
            state.active = true;
        });
        while let Ok(reply) = requests.try_recv() {
            quiesced |= reply.respond(observed);
        }
        if !quiesced {
            respond(peer, process, &wire, observed, frames, &mut phase).await?;
            observed.send_modify(|state| state.phase = phase);
        }
    }
}

async fn respond(
    peer: &mut LivePeer,
    process: &mut Process,
    wire: &watch::Sender<Ledger>,
    observed: &watch::Sender<Received>,
    frames: u64,
    phase: &mut u8,
) -> Result<(), String> {
    let state = wire.borrow().clone();
    if *phase == 0 && frames > 0 && state.inputs == 1 {
        transcript(process, "assistant", "The time is twelve.", false).await?;
        answer(peer, 0..5).await?;
        observed.send_modify(|state| {
            state.first = true;
            state.transcript = Some("The time is twelve.".into());
        });
        *phase = 1;
    }
    if *phase == 1 && state.bytes[0] >= 2880 {
        // Leave a genuinely long reply queued while the 100 ms interruption
        // crosses capture, media and sideband. A 200 ms reply can drain first.
        answer(peer, 5..51).await?;
        *phase = 2;
    }
    if *phase == 2 && state.overlap_base.is_some_and(|base| frames >= base + 5) && state.inputs == 2 {
        if state.stops != 0 || !state.played.is_empty() {
            return Err("M3 overlap: interrupting input arrived after first reply closed".into());
        }
        // The old converter still holds its filter tail. Done precedes
        // the user transcript, followed by audible PCM on the independent RTP
        // path. The pinned V3 parser does not forward speech-start events.
        wire.send_modify(|state| {
            state.cancel = Some(Instant::now());
            state.cut_frames = Some(frames);
        });
        transcript(process, "assistant", "The time", true).await?;
        process
            .send(&json!({"tag":"barge", "events":[{"type":"input_transcript.added", "item":{"text":"What day is it?"}}]}))
            .await?;
        process.expect("provider.sent").await?;
        answer(peer, 51..53).await?;
        *phase = 3;
    }
    replacement(peer, process, wire, observed, frames, phase).await
}

async fn replacement(
    peer: &mut LivePeer,
    process: &mut Process,
    wire: &watch::Sender<Ledger>,
    observed: &watch::Sender<Received>,
    frames: u64,
    phase: &mut u8,
) -> Result<(), String> {
    let state = wire.borrow().clone();
    if *phase == 3
        && state.cut_frames.is_some_and(|base| frames >= base + 5)
        && state.played.len() == 1
        && state.old_sink_pcm.is_some()
    {
        let tail = verify_tail(&state, frames)?;
        wire.send_modify(|state| state.live_tail_frames = tail);
        transcript(process, "assistant", "The day is Monday.", false).await?;
        answer(peer, 53..58).await?;
        observed.send_modify(|state| {
            state.second = true;
            state.transcript = Some("The day is Monday.".into());
        });
        *phase = 4;
    }
    if *phase == 4 && state.bytes[1] >= 2880 {
        // Match M2's eleven-packet finite reply: the remote decoder/resampler
        // retains one packet, so ten packets cannot reach the nine-frame
        // pre-done barrier. Keep the final 9600-byte oracle unchanged.
        answer(peer, 58..64).await?;
        *phase = 5;
    }
    if *phase == 5 && state.bytes[1] >= 8640 {
        transcript(process, "assistant", "The day is Monday.", true).await?;
        *phase = 6;
    }
    Ok(())
}

async fn transcript(process: &mut Process, role: &str, text: &str, done: bool) -> Result<(), String> {
    let value = if done {
        json!({"type":"turn.done", "turn":{"role":role, "transcript":text}})
    } else {
        json!({"type":if role == "user" {"input_transcript.added"} else {"output_transcript.added"},
            "item":{"text":text}})
    };
    process.send(&json!({"tag":"transcript", "events":[value]})).await?;
    process.expect("provider.sent").await?;
    Ok(())
}

fn verify_tail(state: &Ledger, frames: u64) -> Result<u64, String> {
    // Cold call negotiation can evict startup capture. Measure the two exact
    // five-frame inputs from the acknowledged pre-overlap media snapshot.
    let tail = state.cut_frames.and_then(|base| frames.checked_sub(base));
    if state
        .overlap_base
        .zip(state.cut_frames)
        .is_none_or(|(before, cut)| cut != before + 5)
        || tail != Some(5)
        || state.tail_bytes != 3200
        || state.runs != 1
        || state.flushes != 1
    {
        return Err(
            "M3 continuation: expected 5 prefix and 5 tail Live frames, 3200 post-Played bytes and one run/flush"
                .into(),
        );

    }
    tail.ok_or("M3: missing measured Live tail".into())
}

async fn answer(peer: &mut LivePeer, sequences: std::ops::Range<u16>) -> Result<(), String> {
    for sequence in sequences {
        let mut packet = tone::packet(sequence % 11).ok_or("M3: missing tone packet")?;
        packet.sequence = sequence;
        packet.timestamp = u32::from(sequence) * 960;
        peer.send_audio(packet, Instant::now().into_std())
            .await
            .map_err(|_| "M3 Live: reply send failed")?;
    }
    Ok(())
}

async fn drive(peer: &mut LivePeer, deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err("M3 Live: phase deadline elapsed".into());
    }
    let next = peer
        .flush()
        .await
        .map_err(|_| "M3 Live: peer flush failed")?
        .min(deadline.into_std())
        .min((Instant::now() + Duration::from_millis(20)).into_std());
    tokio::select! {
        result = peer.readable() => {
            result.map_err(|_| "M3 Live: peer disconnected")?;
            peer.receive(Instant::now().into_std()).await.map_err(|_| "M3 Live: peer receive failed")?;
        }
        () = tokio::time::sleep_until(next.into()) => { peer.timeout(Instant::now().into_std()).await.map_err(|_| "M3 Live: peer timer failed")?; }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_tail_requires_exact_unflushed_frame_total() {
        let state = Ledger {
            overlap_base: Some(44),
            cut_frames: Some(49),
            tail_bytes: 3200,
            runs: 1,
            flushes: 1,
            ..Ledger::default()
        };
        assert!(verify_tail(&state, 54).is_ok());
        for frames in [49, 53, 55] {
            assert!(verify_tail(&state, frames).is_err());
        }
        let cold = Ledger {
            overlap_base: Some(9),
            cut_frames: Some(14),
            ..state
        };
        assert!(verify_tail(&cold, 19).is_ok());
        assert!(verify_tail(&cold, 18).is_err());
        assert!(verify_tail(&cold, 20).is_err());
        assert!(
            verify_tail(
                &Ledger {
                    cut_frames: Some(15),
                    ..cold.clone()
                },
                19
            )
            .is_err()
        );
        assert!(
            verify_tail(
                &Ledger {
                    overlap_base: None,
                    ..cold
                },
                19
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn stopped_receiver_rejects_snapshot_instead_of_reusing_retained_frames() {
        let (observed, _) = watch::channel(Received {
            frames: 50,
            active: true,
            ..Received::default()
        });
        let (snapshots, requests) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            drop(requests);
            Ok(())
        });
        let live = Live {
            origin: String::new(),
            observed,
            snapshots,
            stop: None,
            task,
            closing: Arc::new(AtomicBool::new(false)),
        };
        assert_eq!(live.snapshot().await.unwrap_err(), "M3 mute: Live observer ended");
        assert_eq!(live.observed.borrow().frames, 50);
    }

    #[test]
    fn ended_receiver_cannot_supply_zero_delta_mute_evidence() {
        let mut state = Received {
            frames: 50,
            active: true,
            ..Received::default()
        };
        assert_eq!(state.snapshot().unwrap(), 50);
        state.active = false;
        assert_eq!(state.snapshot().unwrap_err(), "M3 mute: Live observer ended or failed");
        state.active = true;
        state.failure = Some("disconnected".into());
        assert!(state.snapshot().is_err());
    }
}
