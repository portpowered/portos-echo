//! M5 local V3/Responses service and real media remote, owned across linking.
use super::super::process::Process;
use echo_live::LivePeer;
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

pub struct Peers {
    pub origin: String,
    pub events: mpsc::Sender<()>,
    pub observed: watch::Receiver<Value>,
    clock: tokio::time::Interval,
    stop: mpsc::Sender<Option<tokio::sync::oneshot::Sender<()>>>,
    task: Option<JoinHandle<Result<Value, String>>>,
}
impl Peers {
    pub async fn bind(python: &str) -> Result<Self, String> {
        let mut command = tokio::process::Command::new(python);
        command
            .arg("-u")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/m4/codex/provider.py"));
        let mut process = Process::start(&mut command)?;
        let ready = process.expect("provider.ready").await?;
        let tool = echo_spoof_mcp::catalog()[0].clone();
        process
            .send(&json!({"mcp":true,"tools":[{"name":"test_echo",
            "description":tool["description"],"inputSchema":tool["inputSchema"]}]}))
            .await?;
        let origin = ready["origin"]
            .as_str()
            .ok_or("M5: missing provider origin")?
            .to_owned();
        let (events, receive) = mpsc::channel(2);
        let (stop, stopping) = mpsc::channel(1);
        let (observe, observed) = watch::channel(json!({"delegations":[],"appends":[],"requests":[]}));
        let task = tokio::spawn(Box::pin(owned(process, receive, stopping, observe)));
        let mut clock = tokio::time::interval(std::time::Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Ok(Self {
            origin,
            events,
            observed,
            clock,
            stop,
            task: Some(task),
        })
    }
    pub async fn tick(&mut self) -> Result<(), String> {
        self.clock.tick().await;
        Ok(())
    }
    pub async fn shutdown(mut self) -> Result<Value, String> {
        let _ = self.stop.send(None).await;
        let value = self
            .task
            .take()
            .ok_or("M5: missing provider owner")?
            .await
            .map_err(|_| "M5: provider task failed")??;
        validate(&value)?;
        Ok(value)
    }
    pub async fn quiesce(&self) -> Result<(), String> {
        let (done, finished) = tokio::sync::oneshot::channel();
        self.stop
            .send(Some(done))
            .await
            .map_err(|_| "M5: provider control ended")?;
        finished.await.map_err(|_| "M5: media did not quiesce".into())
    }
}

fn validate(value: &Value) -> Result<(), String> {
    let appends = value["appends"].as_array().ok_or("M5: missing append observations")?;
    let ids = ["m5-linked", "m5-replayed"]
        .into_iter()
        .take(appends.len())
        .collect::<Vec<_>>();
    let calls = if ids.is_empty() {
        json!([])
    } else {
        json!([{
        "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
        "model":"gpt-live-1-codex", "alpha":"quicksilver=v2"}])
    };
    let models = (0..ids.len() * 2)
        .map(|index| json!({"tools":["test_echo"],"outputs":index.div_ceil(2)}))
        .collect::<Vec<_>>();
    let expected = json!({"event":"provider.snapshot", "calls":calls,"sidebands":u8::from(!ids.is_empty()),
        "external":0, "delegations":ids, "appends":ids,"transcripts":[],"models":models});
    if value != &expected {
        return Err("M5: exact pinned provider observations disagree".into());
    }
    Ok(())
}
impl Drop for Peers {
    fn drop(&mut self) {
        let _ = self.stop.try_send(None);
    }
}
async fn owned(
    mut process: Process,
    mut events: mpsc::Receiver<()>,
    mut stop: mpsc::Receiver<Option<tokio::sync::oneshot::Sender<()>>>,
    observe: watch::Sender<Value>,
) -> Result<Value, String> {
    let mut peer = None;
    let result = drive(&mut process, &mut events, &mut stop, &observe, &mut peer).await;
    if let Some(peer) = &mut peer {
        peer.close();
    }
    let cleanup = process.close().await;
    result.and_then(|value| cleanup.map(|()| value))
}
async fn drive(
    process: &mut Process,
    events: &mut mpsc::Receiver<()>,
    stop: &mut mpsc::Receiver<Option<tokio::sync::oneshot::Sender<()>>>,
    observe: &watch::Sender<Value>,
    peer: &mut Option<LivePeer>,
) -> Result<Value, String> {
    let offer = loop {
        tokio::select! {
            offer = process.expect("provider.offer") => break Some(offer?),
            control = stop.recv() => match control {
                Some(Some(done)) => { let _ = done.send(()); },
                Some(None) | None => break None,
            },
        }
    };
    if let Some(offer) = offer {
        let mut remote = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
            .await
            .map_err(|_| "M5: media bind failed")?;
        let answer = remote
            .answer(offer["offer"].as_str().ok_or("M5: missing offer")?)
            .await
            .map_err(|_| "M5: invalid SDP")?;
        process.send(&json!({"answer":answer})).await?;
        *peer = Some(remote);
        conversation(process, events, stop, observe, peer.as_mut().ok_or("M5: missing peer")?).await?;
    }
    process.send(&json!({"snapshot":true})).await?;
    loop {
        let value = process.line().await?;
        if value["event"] == "provider.snapshot" {
            return Ok(value);
        }
        observation(observe, &value)?;
    }
}
async fn conversation(
    process: &mut Process,
    events: &mut mpsc::Receiver<()>,
    stop: &mut mpsc::Receiver<Option<tokio::sync::oneshot::Sender<()>>>,
    observe: &watch::Sender<Value>,
    peer: &mut LivePeer,
) -> Result<(), String> {
    let mut quiesced = false;
    loop {
        // Keep partial JSONL intact across remote-media ticks and controls.
        let mut delegate = false;
        let value = {
            let line = process.line();
            tokio::pin!(line);
            loop {
                tokio::select! {
                    value = &mut line => break Some(value?),
                    result = drive_media(peer), if !quiesced => result?,
                    event = events.recv() => { event.ok_or("M5: delegation channel closed")?; delegate = true; break None; },
                    control = stop.recv() => match control {
                        Some(Some(done)) => { quiesced = true; let _ = done.send(()); },
                        Some(None) | None => return Ok(()),
                    },
                }
            }
        };
        if delegate {
            process.send(&json!({"transcripts":true})).await?;
        }
        if let Some(value) = value {
            observation(observe, &value)?;
        }
    }
}
fn observation(observe: &watch::Sender<Value>, value: &Value) -> Result<(), String> {
    let (key, item) = match value["event"].as_str() {
        Some("provider.delegation") => ("delegations", value["delegation_id"].clone()),
        Some("provider.append") => ("appends", value["delegation_id"].clone()),
        Some("provider.model") => ("requests", value["request"].clone()),
        Some("provider.failure") => return Err(format!("M5: provider contract: {}", value["failure"])),
        _ => return Err("M5: unexpected provider receipt".into()),
    };
    observe.send_modify(|state| {
        if let Some(items) = state[key].as_array_mut() {
            items.push(item);
        }
    });
    Ok(())
}
async fn drive_media(peer: &mut LivePeer) -> Result<(), String> {
    while peer.pop_audio().is_some() {}
    let deadline = peer.flush().await.map_err(|_| "M5: media flush")?;
    tokio::select! {
        result = peer.readable() => {
            result.map_err(|_| "M5: media readiness")?;
            peer.receive(Instant::now().into_std()).await.map_err(|_| "M5: media receive")?;
        }
        () = tokio::time::sleep_until(deadline.into()) => peer.timeout(Instant::now().into_std()).await.map_err(|_| "M5: media timeout")?,
    }
    Ok(())
}
