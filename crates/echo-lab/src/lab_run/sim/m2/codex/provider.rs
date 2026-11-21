//! One local provider process and one single-owner Opus remote per M2 case.
pub(super) mod driver;
mod stream;
use crate::lab_run::sim::process::Process;
use echo_live::LivePeer;
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::Instant,
};

pub(super) enum Control {
    Transcripts,
    Reply,
    Tail,
    Quiesce(oneshot::Sender<()>),
    Close,
    #[cfg(test)]
    NextCapture,
    #[cfg(test)]
    Maintenance(oneshot::Sender<()>),
}
pub(in crate::lab_run::sim) struct Provider {
    pub origin: String,
    commands: mpsc::Sender<Control>,
    ticks: mpsc::Receiver<()>,
    task: Option<JoinHandle<Result<Value, String>>>,
}
impl Provider {
    #[cfg(test)]
    pub(super) async fn start_retained(python: &str) -> Result<Self, String> {
        Self::start_mode(python, true, None, false, true).await
    }
    #[cfg(test)]
    pub(super) async fn maintained(&self) {
        let (send, receive) = oneshot::channel();
        self.commands.send(Control::Maintenance(send)).await.unwrap();
        receive.await.unwrap();
    }
    #[cfg(test)]
    pub(super) async fn next_capture(&self) {
        self.commands.send(Control::NextCapture).await.unwrap();
    }
    pub async fn start(python: &str) -> Result<Self, String> {
        Self::start_mode(python, false, None, false, false).await
    }
    pub async fn start_m1(python: &str) -> Result<Self, String> {
        Self::start_mode(python, true, None, false, false).await
    }
    #[cfg(test)]
    pub(super) async fn start_fault(python: &str, fault: &str) -> Result<Self, String> {
        Self::start_mode(python, false, Some(fault), false, false).await
    }
    #[cfg(test)]
    pub(super) async fn start_quiet_edges(python: &str) -> Result<Self, String> {
        Self::start_mode(python, false, None, true, false).await
    }
    async fn start_mode(
        python: &str,
        repeated: bool,
        fault: Option<&str>,
        quiet_edges: bool,
        retained: bool,
    ) -> Result<Self, String> {
        let mut command = tokio::process::Command::new(python);
        command
            .arg("-u")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/m2/codex/provider.py"))
            .args(fault);
        let mut process = Process::start(&mut command)?;
        let ready = process.expect("provider.ready").await?;
        let origin = ready["origin"]
            .as_str()
            .ok_or("m2: missing provider origin")?
            .to_owned();
        let (commands, receiver) = mpsc::channel(2);
        let (ticks, clock) = mpsc::channel(1);
        let task = tokio::spawn(owned(process, receiver, ticks, repeated, quiet_edges, retained));
        Ok(Self {
            origin,
            commands,
            ticks: clock,
            task: Some(task),
        })
    }
    pub async fn tick(&mut self) -> Result<(), String> {
        self.ticks.recv().await.ok_or("m2: provider media driver ended".into())
    }
    pub async fn reply(&self) -> Result<(), String> {
        self.commands
            .send(Control::Reply)
            .await
            .map_err(|_| "m2: reply gate closed".into())
    }
    pub async fn tail(&self) -> Result<(), String> {
        self.commands
            .send(Control::Tail)
            .await
            .map_err(|_| "m2: provider control closed".into())
    }
    pub async fn transcripts(&self) -> Result<(), String> {
        self.commands
            .send(Control::Transcripts)
            .await
            .map_err(|_| "m2: provider control closed".into())
    }
    pub async fn quiesce(&self) -> Result<(), String> {
        let (send, receive) = oneshot::channel();
        self.commands
            .send(Control::Quiesce(send))
            .await
            .map_err(|_| "m2: provider control closed")?;
        receive.await.map_err(|_| "m2: provider media did not quiesce".into())
    }
    pub async fn close(mut self) -> Result<Value, String> {
        let _ = self.commands.send(Control::Close).await;
        self.task
            .take()
            .ok_or("m2: provider task missing")?
            .await
            .map_err(|_| "m2: provider task failed")?
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        // A cancelled journey drops its sender, waking the task's receiver.
        // Normal close joins the task and reaps the process explicitly.
        let _ = self.commands.try_send(Control::Close);
    }
}

async fn owned(
    mut process: Process,
    mut commands: mpsc::Receiver<Control>,
    ticks: mpsc::Sender<()>,
    repeated: bool,
    quiet_edges: bool,
    retained: bool,
) -> Result<Value, String> {
    let mut peer = None;
    let result = if repeated {
        repeat(&mut process, &mut commands, &mut peer, retained).await
    } else {
        drive(&mut process, &mut commands, &ticks, &mut peer, quiet_edges).await
    };
    if let Some(peer) = &mut peer {
        peer.close();
    }
    let closed = process.close().await;
    result.and_then(|snapshot| closed.map(|()| snapshot))
}

async fn drive(
    process: &mut Process,
    commands: &mut mpsc::Receiver<Control>,
    ticks: &mpsc::Sender<()>,
    peer: &mut Option<LivePeer>,
    quiet_edges: bool,
) -> Result<Value, String> {
    let offer = admit(process, commands).await?;
    let Some(offer) = offer else {
        process.send(&json!({"snapshot":true})).await?;
        return process.expect("provider.snapshot").await;
    };
    negotiate(process, peer, &offer).await?;
    let mut quiesced = peer.is_none();
    let mut reply = false;
    let mut started = false;
    let mut released = false;
    let mut tail = false;
    let mut stream = stream::Stream::default();
    let mut clock = tokio::time::interval(std::time::Duration::from_millis(20));
    loop {
        let work = async {
            if quiesced {
                std::future::pending().await
            } else {
                step(peer, &mut reply, &mut tail, quiet_edges, &mut stream).await
            }
        };
        let progress = progress(work, process, commands, ticks, &mut clock, quiesced).await?;
        match progress {
            Progress::Media(received, tail_released, reply_released) => {
                tail |= tail_released;
                released |= reply_released;
                started |= received > 0;
                if started && released {
                    reply = true;
                    released = false;
                }
            }
            Progress::Quiesced => quiesced = true,
            Progress::Closed => break,
        }
    }
    process.send(&json!({"snapshot":true})).await?;
    process.expect("provider.snapshot").await
}

enum Progress {
    Media(usize, bool, bool),
    Quiesced,
    Closed,
}

/// Clock ticks and transcript controls must not cancel a partial media step.
/// Only quiescing or closing deliberately abandons that operation.
async fn progress(
    work: impl std::future::Future<Output = Result<usize, String>>,
    process: &mut Process,
    commands: &mut mpsc::Receiver<Control>,
    ticks: &mpsc::Sender<()>,
    clock: &mut tokio::time::Interval,
    quiesced: bool,
) -> Result<Progress, String> {
    tokio::pin!(work);
    let mut tail = false;
    let mut reply = false;
    loop {
        tokio::select! {
            result = &mut work => return result.map(|received| Progress::Media(received, tail, reply)),
            _ = clock.tick(), if !quiesced => { let _ = ticks.try_send(()); },
            control = commands.recv() => match control {
                Some(Control::Tail) => tail = true,
                Some(Control::Reply) => reply = true,
                Some(Control::Transcripts) => process.send(&json!({"transcripts":true})).await?,
                Some(Control::Quiesce(done)) => {
                    let _ = done.send(());
                    return Ok(Progress::Quiesced);
                }
                Some(Control::Close) | None => return Ok(Progress::Closed),
                #[cfg(test)]
                Some(Control::NextCapture | Control::Maintenance(_)) => return Err("m2: capture-only control".into()),
            },
        }
    }
}

/// M1 preserves its capture-only oracle: no reply RTP is generated.
/// Native completion follows twelve microphone frames received by the remote.
/// New offers replace the retained peer after the prior call's idle stop.
async fn repeat(
    process: &mut Process,
    commands: &mut mpsc::Receiver<Control>,
    peer: &mut Option<LivePeer>,
    retained: bool,
) -> Result<Value, String> {
    let mut capture = None;
    let mut servicing = false;
    let mut reply = false;
    let mut tail = false;
    let mut stream = stream::Stream::default();
    loop {
        tokio::select! {
            offer = process.expect("provider.offer") => {
                if let Some(peer) = peer.as_mut() { peer.close(); }
                negotiate(process, peer, &offer?).await?;
                stream = stream::Stream::default();
                capture = Some(0);
                servicing = true;
            },
            result = driver::advance(step(peer, &mut reply, &mut tail, false, &mut stream), commands, &mut capture, &mut servicing, retained) => {
                match result? {
                    Some(true) => {
                    process.send(&json!({"transcripts":true})).await?;
                    },
                    Some(false) => {},
                    None => break,
                }
            },
        }
    }
    process.send(&json!({"summary":true})).await?;
    process.expect("provider.snapshot").await
}

async fn admit(process: &mut Process, commands: &mut mpsc::Receiver<Control>) -> Result<Option<Value>, String> {
    // This read is cancelled only when abandoning the entire provider, never
    // for an RTP tick or an operation that would resume a partial JSONL read.
    tokio::select! {
        value = process.expect("provider.offer") => value.map(Some),
        control = commands.recv() => {
            if let Some(Control::Quiesce(done)) = control { let _ = done.send(()); }
            Ok(None)
        },
    }
}

async fn negotiate(process: &mut Process, peer: &mut Option<LivePeer>, offer: &Value) -> Result<(), String> {
    let mut remote = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .map_err(|_| "m2: remote media bind")?;
    let answer = remote
        .answer(offer["offer"].as_str().ok_or("m2: missing real offer")?)
        .await
        .map_err(|_| "m2: real SDP negotiation failed")?;
    process.send(&json!({"answer":answer})).await?;
    *peer = Some(remote);
    Ok(())
}

async fn step(
    peer: &mut Option<LivePeer>,
    reply: &mut bool,
    tail: &mut bool,
    quiet_edges: bool,
    stream: &mut stream::Stream,
) -> Result<usize, String> {
    let mut received = 0;
    let peer = peer.as_mut().ok_or("m2: missing remote media")?;
    if peer.ready() {
        while peer.pop_audio().is_some() {
            received += 1;
        }
        let sequences = if std::mem::take(reply) {
            0..5
        } else if std::mem::take(tail) {
            5..11
        } else {
            0..0
        };
        // Keep each burst below the production ingress capacity. The tail is
        // released only after three actual PCM writes prove initial consumption.
        for sequence in sequences {
            let frame = stream.packet(sequence, quiet_edges)?;
            peer.send_audio(frame, Instant::now().into_std())
                .await
                .map_err(|_| "m2: synthetic reply media")?;
        }
    }
    let next = Instant::from_std(peer.flush().await.map_err(|_| "m2: remote media flush")?);
    tokio::select! {
        ready = peer.readable() => {
            ready.map_err(|_| "m2: remote media readiness")?;
            peer.receive(Instant::now().into_std()).await.map_err(|_| "m2: remote media receive")?;
        },
        () = tokio::time::sleep_until(next) => peer.timeout(Instant::now().into_std()).await.map_err(|_| "m2: remote media timeout")?,
    }
    Ok(received)
}

pub(super) fn valid(snapshot: &Value) -> bool {
    snapshot
        == &json!({"event":"provider.snapshot", "calls":[{
        "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
        "model":"gpt-live-1.1-codex", "alpha":"quicksilver=v2", "nominated_key":true,
        "account_header":null}], "sidebands":1, "external":0, "refreshes":0})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn t3_media_progress_survives_clock_ticks_and_transcript_control() {
        let python = "python3";
        let mut command = tokio::process::Command::new(python);
        command.args([
            "-u",
            "-c",
            "import sys\nfor line in sys.stdin: print(line, end='', flush=True)",
        ]);
        let mut process = Process::start(&mut command).unwrap();
        let (commands, mut receive) = mpsc::channel(2);
        let (ticks, mut clock_ticks) = mpsc::channel(1);
        let (finish, finished) = oneshot::channel();
        let mut clock = tokio::time::interval(std::time::Duration::from_millis(20));
        let starts = std::sync::atomic::AtomicUsize::new(0);
        let work = async {
            starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            finished.await.unwrap();
            Ok(11)
        };
        let controls = async {
            for _ in 0..3 {
                clock_ticks.recv().await.unwrap();
            }
            commands.send(Control::Tail).await.unwrap();
            commands.send(Control::Transcripts).await.unwrap();
            clock_ticks.recv().await.unwrap();
            finish.send(()).unwrap();
        };
        let (result, ()) = tokio::join!(
            progress(work, &mut process, &mut receive, &ticks, &mut clock, false),
            controls
        );
        assert!(matches!(result.unwrap(), Progress::Media(11, true, false)));
        assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(process.line().await.unwrap(), json!({"transcripts":true}));
        // The external process exits on wall time; only media progress used fake time.
        tokio::time::resume();
        assert_eq!(process.close().await, Ok(()));
    }
}
