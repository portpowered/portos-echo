//! Single owner of the session provider's JSONL, sideband and Opus media.
use crate::lab_run::sim::process::Process;
use echo_live::LivePeer;
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

pub(super) enum Control {
    Delegate(usize),
    /// Tone payload indices of the current reply; transport identities continue.
    Audio(std::ops::Range<u16>),
    Transcripts(usize),
    Events(&'static str, Vec<Value>),
    TimeQuestion(&'static str),
    TimeQuestionOnAudio(&'static str, oneshot::Sender<()>),
    MediaQuestion(&'static str),
    ReleaseGoodbye,
    Quiesce(oneshot::Sender<()>),
    Close,
}

pub(super) struct Provider {
    pub origin: String,
    pub observed: watch::Receiver<Value>,
    commands: mpsc::Sender<Control>,
    ticks: mpsc::Receiver<()>,
    task: Option<JoinHandle<Result<Value, String>>>,
}

impl Provider {
    pub async fn start(python: &str, script: &Value) -> Result<Self, String> {
        let mut command = tokio::process::Command::new(python);
        command
            .arg("-u")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/session/provider.py"));
        let mut process = Process::start(&mut command)?;
        let ready = process.expect("provider.ready").await?;
        process.send(&json!({"script":script})).await?;
        let origin = ready["origin"]
            .as_str()
            .ok_or("session: missing provider origin")?
            .to_owned();
        let (commands, receiver) = mpsc::channel(2);
        let (ticks, clock) = mpsc::channel(1);
        let (observe, observed) = watch::channel(
            json!({"delegations":[],"appends":[],"models":[],"requests":[],"events_sent":[],"waiting":[],"time_turns":[]}),
        );
        let task = tokio::spawn(owned(process, receiver, ticks, observe));
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
            .map_err(|_| "session: provider control closed".into())
    }

    /// One media cadence tick; the owner keeps its peer serviced meanwhile.
    pub async fn tick(&mut self) -> Result<(), String> {
        self.ticks
            .recv()
            .await
            .ok_or("session: provider media driver ended".into())
    }

    pub fn count(&self, key: &str) -> usize {
        self.observed.borrow()[key].as_array().map_or(0, Vec::len)
    }

    pub async fn quiesce(&self) -> Result<(), String> {
        let (done, finished) = oneshot::channel();
        self.control(Control::Quiesce(done)).await?;
        finished.await.map_err(|_| "session: media did not quiesce".into())
    }

    pub async fn arm_time_question(&self, question: &'static str) -> Result<(), String> {
        let (armed, ready) = oneshot::channel();
        self.control(Control::TimeQuestionOnAudio(question, armed)).await?;
        ready
            .await
            .map_err(|_| "session: time question media trigger unavailable".into())
    }

    pub async fn close(mut self) -> Result<Value, String> {
        let _ = self.commands.send(Control::Close).await;
        self.task
            .take()
            .ok_or("session: provider task missing")?
            .await
            .map_err(|_| "session: provider task failed")?
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        let _ = self.commands.try_send(Control::Close);
    }
}

async fn owned(
    mut process: Process,
    mut commands: mpsc::Receiver<Control>,
    ticks: mpsc::Sender<()>,
    observed: watch::Sender<Value>,
) -> Result<Value, String> {
    let mut peer = None;
    let result = drive(&mut process, &mut commands, &ticks, &observed, &mut peer).await;
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
) -> Result<Value, String> {
    let offer = tokio::select! {
        offer = process.expect("provider.offer") => Some(offer?),
        control = commands.recv() => {if let Some(Control::Quiesce(done)) = control {let _ = done.send(());} None},
    };
    if let Some(offer) = offer {
        let mut remote = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
            .await
            .map_err(|_| "session: media bind")?;
        let answer = remote
            .answer(offer["offer"].as_str().ok_or("session: missing offer")?)
            .await
            .map_err(|_| "session: SDP answer")?;
        process.send(&json!({"answer":answer})).await?;
        *peer = Some(remote);
        conversation(
            process,
            commands,
            ticks,
            observed,
            peer.as_mut().ok_or("session: missing peer")?,
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
) -> Result<(), String> {
    let mut quiesced = false;
    let mut sequence = Sequence::default();
    let mut question = None;
    // Service the remote at the media frame cadence even without UDP traffic.
    let mut clock = tokio::time::interval(std::time::Duration::from_millis(20));
    loop {
        let mut action = None;
        // Preserve partial JSONL across every media tick.
        let value = {
            let line = process.line();
            tokio::pin!(line);
            loop {
                tokio::select! {
                    value = &mut line => break Some(value?),
                    result = media_step(peer, &mut question), if !quiesced => {
                        if let Some(text) = result? {
                            action = Some(Control::MediaQuestion(text));
                            break None;
                        }
                    },
                    _ = clock.tick(), if !quiesced => {let _ = ticks.try_send(());},
                    control = commands.recv() => { action = control; break None; },
                }
            }
        };
        if let Some(value) = value {
            observation(observed, &value)?;
            continue;
        }
        if !control(action, process, peer, &mut sequence, &mut question, &mut quiesced).await? {
            return Ok(());
        }
    }
}

async fn control(
    action: Option<Control>,
    process: &mut Process,
    peer: &mut LivePeer,
    sequence: &mut Sequence,
    question: &mut Option<&'static str>,
    quiesced: &mut bool,
) -> Result<bool, String> {
    match action {
        Some(Control::Delegate(turn)) => process.send(&json!({"delegate":turn})).await?,
        Some(Control::Transcripts(turn)) => process.send(&json!({"transcripts":turn})).await?,
        Some(Control::Events(tag, events)) => process.send(&json!({"tag":tag,"events":events})).await?,
        Some(Control::TimeQuestion(text)) => process.send(&json!({"time_question":text})).await?,
        Some(Control::MediaQuestion(text)) => process.send(&json!({"media_question":text})).await?,
        Some(Control::TimeQuestionOnAudio(text, armed)) => arm(question, text, armed)?,
        Some(Control::ReleaseGoodbye) => process.send(&json!({"release_goodbye":true})).await?,
        Some(Control::Audio(indices)) => sequence.send(peer, indices).await?,
        Some(Control::Quiesce(done)) => {
            *quiesced = true;
            let _ = done.send(());
        }
        Some(Control::Close) | None => return Ok(false),
    }
    Ok(true)
}

fn arm(question: &mut Option<&'static str>, text: &'static str, armed: oneshot::Sender<()>) -> Result<(), String> {
    if question.replace(text).is_some() {
        return Err("session: overlapping time questions".into());
    }
    let _ = armed.send(());
    Ok(())
}

async fn media_step(peer: &mut LivePeer, question: &mut Option<&'static str>) -> Result<Option<&'static str>, String> {
    if step(peer).await? {
        Ok(question.take())
    } else {
        Ok(None)
    }
}

fn observation(observed: &watch::Sender<Value>, value: &Value) -> Result<(), String> {
    let (key, item) = match value["event"].as_str() {
        Some("provider.failure") => {
            return Err(format!(
                "session: provider contract failure: {}",
                value["failure"].as_str().unwrap_or("unspecified")
            ));
        }
        Some("provider.delegation") => {
            observed.send_modify(|state| {
                if let Some(requests) = state["requests"].as_array_mut() {
                    requests.push(value["request"].clone());
                }
            });
            ("delegations", value["delegation_id"].clone())
        }
        Some("provider.append") => ("appends", value["delegation_id"].clone()),
        Some("provider.model") => ("models", json!({"turn":value["turn"],"outputs":value["outputs"]})),
        Some("provider.transcripts" | "provider.heard") => return Ok(()),
        Some("provider.events_sent") => ("events_sent", value["tag"].clone()),
        Some("provider.waiting") => ("waiting", value["turn"].clone()),
        Some("provider.time_turn") => ("time_turns", value.clone()),
        _ => return Err("session: provider contract failure".into()),
    };
    observed.send_modify(|state| {
        if let Some(items) = state[key].as_array_mut() {
            items.push(item);
        }
    });
    Ok(())
}

/// One RTP sequence space per negotiated peer, across all of its replies.
#[derive(Default)]
struct Sequence {
    next: u16,
}

impl Sequence {
    async fn send(&mut self, peer: &mut LivePeer, indices: std::ops::Range<u16>) -> Result<(), String> {
        if !peer.ready() {
            return Err("session: reply before media readiness".into());
        }
        for index in indices {
            let mut frame =
                crate::lab_run::sim::m2::codex::tone::packet(index).ok_or("session: invalid tone sequence")?;
            frame.sequence = self.next;
            frame.timestamp = u32::from(self.next) * 960;
            self.next = self.next.wrapping_add(1);
            peer.send_audio(frame, Instant::now().into_std())
                .await
                .map_err(|_| "session: reply media")?;
        }
        Ok(())
    }
}

async fn step(peer: &mut LivePeer) -> Result<bool, String> {
    let next = Instant::from_std(peer.flush().await.map_err(|_| "session: media flush")?);
    tokio::select! {
        ready = peer.readable() => {
            ready.map_err(|_| "session: media readiness")?;
            peer.receive(Instant::now().into_std()).await.map_err(|_| "session: media receive")?;
        },
        () = tokio::time::sleep_until(next) => peer.timeout(Instant::now().into_std()).await.map_err(|_| "session: media timeout")?,
    }
    let mut heard = false;
    while peer.pop_audio().is_some() {
        heard = true;
    }
    Ok(heard)
}
