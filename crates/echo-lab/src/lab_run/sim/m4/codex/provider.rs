//! Single owner of provider JSONL, sideband observations, and real Opus media.
use crate::lab_run::sim::process::Process;
use echo_live::LivePeer;
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

pub(super) enum Control {
    Delegate,
    Complete,
    Reply,
    Quiesce(oneshot::Sender<()>),
    Close,
}
pub(super) struct Provider {
    pub origin: String,
    pub observed: watch::Receiver<Value>,
    pub(super) commands: mpsc::Sender<Control>,
    ticks: mpsc::Receiver<()>,
    task: Option<JoinHandle<Result<Value, String>>>,
}
impl Provider {
    pub async fn start_mode(python: &str, negative: bool, emotion: bool) -> Result<Self, String> {
        let mut command = tokio::process::Command::new(python);
        command
            .arg("-u")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/m4/codex/provider.py"));
        let mut process = Process::start(&mut command)?;
        let ready = process.expect("provider.ready").await?;
        process
            .send(&json!({"tools": catalog(), "negative":negative,"emotion":emotion}))
            .await?;
        let origin = ready["origin"]
            .as_str()
            .ok_or("m4: missing provider origin")?
            .to_owned();
        let (commands, receiver) = mpsc::channel(2);
        let (ticks, clock) = mpsc::channel(1);
        let (observe, observed) = watch::channel(json!({"delegations":[],"appends":[]}));
        let task = tokio::spawn(owned(process, receiver, ticks, observe, emotion));
        Ok(Self {
            origin,
            observed,
            commands,
            ticks: clock,
            task: Some(task),
        })
    }
    pub async fn control(&self, control: Control) -> Result<(), String> {
        self.commands
            .send(control)
            .await
            .map_err(|_| "m4: provider control closed".into())
    }
    pub async fn tick(&mut self) -> Result<(), String> {
        self.ticks.recv().await.ok_or("m4: provider media driver ended".into())
    }
    pub async fn close(mut self) -> Result<Value, String> {
        let _ = self.commands.send(Control::Close).await;
        self.task
            .take()
            .ok_or("m4: provider task missing")?
            .await
            .map_err(|_| "m4: provider task failed")?
    }
    pub async fn quiesce(&self) -> Result<(), String> {
        let (done, finished) = oneshot::channel();
        self.control(Control::Quiesce(done)).await?;
        finished.await.map_err(|_| "m4: media did not quiesce".into())
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        let _ = self.commands.try_send(Control::Close);
    }
}

fn catalog() -> Value {
    Value::Array(echo_spoof_mcp::catalog().as_array().into_iter().flatten().enumerate().map(|(index, tool)| {
        json!({"name":(["test_echo", "test_wait", "test_requires_approval", "ui_show_emotion"][index]),"description":tool["description"],"inputSchema":tool["inputSchema"]})
    }).collect())
}

async fn owned(
    mut process: Process,
    mut commands: mpsc::Receiver<Control>,
    ticks: mpsc::Sender<()>,
    observed: watch::Sender<Value>,
    emotion: bool,
) -> Result<Value, String> {
    let mut peer = None;
    let result = drive(&mut process, &mut commands, &ticks, &observed, &mut peer, emotion).await;
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
    observed: &watch::Sender<Value>,
    peer: &mut Option<LivePeer>,
    emotion: bool,
) -> Result<Value, String> {
    let offer = tokio::select! {
        offer = process.expect("provider.offer") => Some(offer?),
        control = commands.recv() => {if let Some(Control::Quiesce(done))=control {let _=done.send(());} None},
    };
    if let Some(offer) = offer {
        let mut remote = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
            .await
            .map_err(|_| "m4: media bind")?;
        let answer = remote
            .answer(offer["offer"].as_str().ok_or("m4: missing offer")?)
            .await
            .map_err(|_| "m4: SDP answer")?;
        process.send(&json!({"answer":answer})).await?;
        *peer = Some(remote);
        conversation(
            process,
            commands,
            ticks,
            observed,
            peer.as_mut().ok_or("m4: missing peer")?,
            emotion,
        )
        .await?;
    }
    process.send(&json!({"snapshot":true})).await?;
    // A final sideband receipt may precede the requested snapshot.
    loop {
        let value = process.line().await?;
        if value["event"] == "provider.snapshot" {
            return Ok(value);
        }
        observation(observed, &value)?;
    }
}

async fn conversation(
    process: &mut Process,
    commands: &mut mpsc::Receiver<Control>,
    ticks: &mpsc::Sender<()>,
    observed: &watch::Sender<Value>,
    peer: &mut LivePeer,
    emotion: bool,
) -> Result<(), String> {
    let mut reply = false;
    let mut allow_reply = !emotion;
    let mut quiesced = false;
    // Drive the bridge's explicit clock at the media frame cadence even when
    // the remote has no UDP traffic. Sparse ICE timers must not stall its clock.
    let mut clock = tokio::time::interval(std::time::Duration::from_millis(20));
    loop {
        let mut action = None;
        // Preserve partial JSONL across every media tick. Only final close may abandon it.
        let value = {
            let line = process.line();
            tokio::pin!(line);
            loop {
                tokio::select! {
                    value = &mut line => break Some(value?),
                    result = step(peer), if !quiesced => { result?; },
                    _ = clock.tick(), if !quiesced => {let _ = ticks.try_send(());},
                    control = commands.recv() => { action = control; break None; },
                }
            }
        };
        if let Some(value) = value {
            observation(observed, &value)?;
            if value["event"] == "provider.append" {
                if reply {
                    return Err("m4: duplicate native append".into());
                }
                reply = true;
                if allow_reply {
                    send_reply(peer).await?;
                }
            }
        } else {
            match action {
                Some(Control::Delegate) => process.send(&json!({"transcripts":true})).await?,
                Some(Control::Reply) if emotion && !allow_reply => {
                    allow_reply = true;
                    if reply {
                        send_reply(peer).await?;
                    }
                }
                Some(Control::Reply) => return Err("m6: premature reply release".into()),
                Some(Control::Complete) => process.send(&json!({"complete":true})).await?,
                Some(Control::Quiesce(done)) => {
                    quiesced = true;
                    let _ = done.send(());
                }
                Some(Control::Close) | None => return Ok(()),
            }
        }
    }
}

fn observation(observed: &watch::Sender<Value>, value: &Value) -> Result<(), String> {
    if value["event"] == "provider.failure" {
        return Err(format!(
            "m4: provider contract failure: {}",
            value["failure"].as_str().unwrap_or("unspecified")
        ));
    }
    let key = match value["event"].as_str() {
        Some("provider.delegation") => "delegations",
        Some("provider.append") => "appends",
        _ => return Err("m4: provider contract failure".into()),
    };
    observed.send_modify(|state| {
        if let Some(items) = state[key].as_array_mut() {
            items.push(value["delegation_id"].clone());
        }
    });
    Ok(())
}

/// Admit the finite utterance before advancing the bridge clock. RTP spacing
/// stays 20 ms; playback pacing belongs to the delivered bridge, not this host.
async fn send_reply(peer: &mut LivePeer) -> Result<(), String> {
    if !peer.ready() {
        return Err("m4: reply before media readiness".into());
    }
    for sequence in 0..11 {
        peer.send_audio(
            crate::lab_run::sim::m2::codex::tone::packet(sequence).ok_or("m4: invalid tone sequence")?,
            Instant::now().into_std(),
        )
        .await
        .map_err(|_| "m4: reply media")?;
    }
    Ok(())
}

async fn step(peer: &mut LivePeer) -> Result<(), String> {
    while peer.pop_audio().is_some() {}
    let next = Instant::from_std(peer.flush().await.map_err(|_| "m4: media flush")?);
    tokio::select! {
        ready = peer.readable() => {
            ready.map_err(|_| "m4: media readiness")?;
            peer.receive(Instant::now().into_std()).await.map_err(|_| "m4: media receive")?;
        },
        () = tokio::time::sleep_until(next) => peer.timeout(Instant::now().into_std()).await.map_err(|_| "m4: media timeout")?,
    }
    Ok(())
}
