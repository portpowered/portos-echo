//! One local provider process and one single-owner Opus remote per M2 case.
use crate::lab_run::sim::process::Process;
use echo_live::LivePeer;
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::Instant,
};

enum Control {
    Transcripts,
    Quiesce(oneshot::Sender<()>),
    Close,
}
pub(super) struct Provider {
    pub origin: String,
    commands: mpsc::Sender<Control>,
    ticks: mpsc::Receiver<()>,
    task: Option<JoinHandle<Result<Value, String>>>,
}
impl Provider {
    pub async fn start(python: &str) -> Result<Self, String> {
        let mut command = tokio::process::Command::new(python);
        command
            .arg("-u")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/m2/codex/provider.py"));
        let mut process = Process::start(&mut command)?;
        let ready = process.expect("provider.ready").await?;
        let origin = ready["origin"]
            .as_str()
            .ok_or("m2: missing provider origin")?
            .to_owned();
        let (commands, receiver) = mpsc::channel(2);
        let (ticks, clock) = mpsc::channel(1);
        let task = tokio::spawn(owned(process, receiver, ticks));
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
) -> Result<Value, String> {
    let mut peer = None;
    let result = drive(&mut process, &mut commands, &ticks, &mut peer).await;
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
) -> Result<Value, String> {
    let offer = admit(process, commands).await?;
    let Some(offer) = offer else {
        process.send(&json!({"snapshot":true})).await?;
        return process.expect("provider.snapshot").await;
    };
    negotiate(process, peer, &offer).await?;
    let mut quiesced = peer.is_none();
    loop {
        tokio::select! {
            result = step(peer), if !quiesced => { result?; let _ = ticks.try_send(()); },
            control = commands.recv() => match control {
                Some(Control::Transcripts) => process.send(&json!({"transcripts":true})).await?,
                Some(Control::Quiesce(done)) => { quiesced = true; let _ = done.send(()); },
                Some(Control::Close) | None => break,
            },
        }
    }
    process.send(&json!({"snapshot":true})).await?;
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

async fn step(peer: &mut Option<LivePeer>) -> Result<(), String> {
    let peer = peer.as_mut().ok_or("m2: missing remote media")?;
    if peer.ready() {
        while let Some(frame) = peer.pop_audio() {
            peer.send_audio(frame, Instant::now().into_std())
                .await
                .map_err(|_| "m2: remote media echo")?;
        }
    }
    let next = peer.flush().await.map_err(|_| "m2: remote media flush")?;
    tokio::select! {
        ready = peer.readable() => {
            ready.map_err(|_| "m2: remote media readiness")?;
            peer.receive(Instant::now().into_std()).await.map_err(|_| "m2: remote media receive")?;
        },
        () = tokio::time::sleep_until(next.into()) => peer.timeout(Instant::now().into_std()).await.map_err(|_| "m2: remote media timeout")?,
    }
    Ok(())
}

pub(super) fn valid(snapshot: &Value) -> bool {
    snapshot
        == &json!({"event":"provider.snapshot", "calls":[{
        "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
        "model":"gpt-live-1-codex", "alpha":"quicksilver=v2", "nominated_key":true,
        "account_header":null}], "sidebands":1, "external":0, "refreshes":0})
}
