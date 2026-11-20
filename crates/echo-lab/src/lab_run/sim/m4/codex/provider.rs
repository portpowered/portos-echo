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
    Tail,
    Reply,
    Release,
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
    pub async fn start_mode(python: &str, negative: bool, emotion: bool, hold_model: bool) -> Result<Self, String> {
        let mut command = tokio::process::Command::new(python);
        command
            .arg("-u")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/m4/codex/provider.py"));
        let mut process = Process::start(&mut command)?;
        let ready = process.expect("provider.ready").await?;
        process
            .send(&json!({"tools": catalog(), "negative":negative,"emotion":emotion,"hold_model":hold_model}))
            .await?;
        let origin = ready["origin"]
            .as_str()
            .ok_or("m4: missing provider origin")?
            .to_owned();
        let (commands, receiver) = mpsc::channel(2);
        let (ticks, clock) = mpsc::channel(1);
        let (observe, observed) = watch::channel(json!({"delegations":[],"appends":[]}));
        let task = tokio::spawn(owned(process, receiver, ticks, observe, (emotion, hold_model)));
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
    mode: (bool, bool),
) -> Result<Value, String> {
    let mut peer = None;
    let result = drive(&mut process, &mut commands, &ticks, &observed, &mut peer, mode).await;
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
    mode: (bool, bool),
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
            mode,
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
    mode: (bool, bool),
) -> Result<(), String> {
    let (emotion, mut hold_model) = mode;
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
                if std::mem::replace(&mut reply, true) {
                    return Err("m4: duplicate native append".into());
                }
                if allow_reply {
                    observed.send_modify(|state| state["reply_ready"] = json!(true));
                }
            }
        } else {
            match action {
                Some(Control::Delegate) => process.send(&json!({"transcripts":true})).await?,
                Some(Control::Reply) if hold_model => {
                    hold_model = false;
                    process.send(&json!({"release_model":true})).await?;
                }
                Some(Control::Reply) if emotion && !allow_reply => {
                    allow_reply = true;
                    if reply {
                        observed.send_modify(|state| state["reply_ready"] = json!(true));
                    }
                }
                Some(Control::Reply) => return Err("m6: premature reply release".into()),
                Some(Control::Release) if reply && allow_reply => send_reply(peer, 0..5).await?,
                Some(Control::Release) => return Err("m4: release before reply admission".into()),
                Some(Control::Tail) => send_reply(peer, 5..11).await?,
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
    if value["event"] == "provider.model_held" {
        observed.send_modify(|state| state["model_held"] = json!(true));
        return Ok(());
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

/// Each bounded burst fits the production ingress. Release the second only
/// after actual PCM writes prove the first has been consumed.
async fn send_reply(peer: &mut LivePeer, sequences: std::ops::Range<u16>) -> Result<(), String> {
    if !peer.ready() {
        return Err("m4: reply before media readiness".into());
    }
    for sequence in sequences {
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
