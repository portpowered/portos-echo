//! M5 local V3/Responses service and real media remote, owned across linking.
use super::super::process::Process;
use super::mcp_lights::Journey;
use super::{
    mcp_session::{EPOCH, Session},
    mcp_wire::{self, endpoint},
};
use echo_lab::lab_mcp::{Fixture, HomeFault};
use echo_live::LivePeer;
use echo_spoof_mcp::{Options as SpoofOptions, Spoof};
use serde_json::{Value, json};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

impl Session {
    pub(super) async fn start_peers(&mut self, address: std::net::SocketAddr, python: &str) -> Result<String, String> {
        self.fixture = Some(
            Fixture::bind(EPOCH)
                .await
                .map_err(|_| "M5: authorization startup failed")?,
        );
        let fixture = self.fixture.as_ref().ok_or("M5: missing authorization fixture")?;
        if let Some(fault) = self.home_fault {
            fixture
                .set_home_fault(fault)
                .map_err(|_| "M5: home fixture setup failed")?;
        }
        self.endpoints.push(endpoint(fixture.base())?);
        let resource = if self.journey == Journey::Lifecycle {
            let spoof = Spoof::start_authorized(SpoofOptions::default(), Some(fixture.validator()))
                .await
                .map_err(|_| "M5: resource startup failed")?;
            let url = spoof.url().to_owned();
            fixture.set_resource(&url).map_err(|_| "M5: resource binding failed")?;
            self.endpoints.push(endpoint(&url)?);
            self.spoof = Some(spoof);
            url
        } else {
            format!("{}/mcp", fixture.base())
        };
        let live = Peers::bind(python, self.journey, self.home_fault).await?;
        let origin = live.origin.clone();
        self.live = Some(live);
        self.endpoints.push(endpoint(&origin)?);
        mcp_wire::configure(&self.directory, address, &resource, self.journey)?;
        Ok(origin.trim_end_matches('/').into())
    }
}

pub struct Peers {
    pub origin: String,
    pub events: mpsc::Sender<()>,
    pub observed: watch::Receiver<Value>,
    clock: tokio::time::Interval,
    stop: mpsc::Sender<Option<tokio::sync::oneshot::Sender<()>>>,
    task: Option<JoinHandle<Result<Value, String>>>,
    journey: Journey,
    fault: Option<HomeFault>,
}
impl Peers {
    pub async fn bind(python: &str, journey: Journey, fault: Option<HomeFault>) -> Result<Self, String> {
        let mut command = tokio::process::Command::new(python);
        command
            .arg("-u")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/m4/codex/provider.py"));
        let mut process = Process::start_observed(&mut command)?;
        let ready = process.expect("provider.ready").await?;
        process.send(&provider_configuration(journey)).await?;
        let origin = ready["origin"]
            .as_str()
            .ok_or("M5: missing provider origin")?
            .to_owned();
        let (events, receive) = mpsc::channel(2);
        let (stop, stopping) = mpsc::channel(1);
        let (observe, observed) = watch::channel(json!({"delegations":[],"appends":[],"requests":[],"transcripts":[]}));
        let task = tokio::spawn(Box::pin(owned(process, receive, stopping, observe, journey)));
        let mut clock = tokio::time::interval(std::time::Duration::from_millis(20));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Ok(Self {
            origin,
            events,
            observed,
            clock,
            stop,
            task: Some(task),
            journey,
            fault,
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
        if self.journey == Journey::Lifecycle {
            validate(&value)?;
        } else {
            validate_home(&value, self.journey, self.fault)?;
        }
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

fn provider_configuration(journey: Journey) -> Value {
    if journey == Journey::Lifecycle {
        let tool = &echo_spoof_mcp::catalog()[0];
        return json!({"mcp":true,"tools":[{"name":"test_echo",
            "description":tool["description"],"inputSchema":tool["inputSchema"]}]});
    }
    let tools: Vec<_> = ["control", "endpoint_query", "group_query"]
        .into_iter()
        .map(|name| {
            json!({
                "name":name,"description":"Synthetic local home tool",
                "inputSchema":{"type":"object","properties":{},"additionalProperties":false}
            })
        })
        .collect();
    json!({"portos":if journey == Journey::Twin {"twin"} else {"readonly"},"tools":tools})
}

fn validate_home(value: &Value, journey: Journey, fault: Option<HomeFault>) -> Result<(), String> {
    let (requests, reply) = match fault {
        Some(HomeFault::NoRoom) => (2, "I could not find Twin room."),
        Some(HomeFault::AmbiguousRoom) => (2, "Which Twin room did you mean?"),
        Some(HomeFault::ControlUnavailable) => (3, "I could not confirm the Twin room lights were turned off."),
        None if journey == Journey::ReadOnly => (4, "Link Echo to Port OS again to control devices."),
        None => (3, "The Twin room lights are off."),
    };
    let models: Vec<_> = (0..requests).map(|outputs|
        json!({"tools":["control","endpoint_query","group_query","extend_session","end_session"],"outputs":outputs})
    ).collect();
    let expected = json!({"event":"provider.snapshot","calls":[{
        "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
        "model":"gpt-live-1-codex","alpha":"quicksilver=v2"}],"sidebands":1,"external":0,
        "delegations":["m5-home"],"appends":["m5-home"],"models":models,
        "transcripts":[{"role":"user","text":"Computer, turn off twin room lights"},{"role":"assistant","text":reply}]});
    if value != &expected {
        return Err("M5: exact Port OS provider observations disagree".into());
    }
    Ok(())
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
        .map(|index| json!({"tools":["test_echo","extend_session","end_session"],"outputs":index.div_ceil(2)}))
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
    journey: Journey,
) -> Result<Value, String> {
    let mut peer = None;
    let result = drive(&mut process, &mut events, &mut stop, &observe, &mut peer, journey).await;
    if let Some(peer) = &mut peer {
        peer.close();
    }
    let cleanup = process
        .close()
        .await
        .map_err(|error| format!("{error}; {}", process.exit_summary()));
    result.and_then(|value| cleanup.map(|()| value))
}
async fn drive(
    process: &mut Process,
    events: &mut mpsc::Receiver<()>,
    stop: &mut mpsc::Receiver<Option<tokio::sync::oneshot::Sender<()>>>,
    observe: &watch::Sender<Value>,
    peer: &mut Option<LivePeer>,
    journey: Journey,
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
        conversation(
            process,
            events,
            stop,
            observe,
            peer.as_mut().ok_or("M5: missing peer")?,
            journey,
        )
        .await?;
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
    journey: Journey,
) -> Result<(), String> {
    let mut quiesced = false;
    let mut reply = None;
    let mut cadence = tokio::time::interval(std::time::Duration::from_millis(20));
    cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
                    _ = cadence.tick(), if reply.is_some() && !quiesced => {
                        if !reply_packet(peer, &mut reply).await? {
                            break None;
                        }
                    },
                    event = events.recv() => { event.ok_or("M5: delegation channel closed")?; delegate = true; break None; },
                    control = stop.recv() => match control {
                        Some(Some(done)) => { quiesced = true; let _ = done.send(()); },
                        Some(None) | None => return Ok(()),
                    },
                }
            }
        };
        let Some(value) = value else {
            let command = if delegate { "transcripts" } else { "complete" };
            process.send(&json!({(command):true})).await?;
            continue;
        };
        observation(observe, &value)?;
        if journey != Journey::Lifecycle && value["event"] == "provider.append" {
            reply = Some(0);
            cadence.reset();
        }
    }
}
async fn reply_packet(peer: &mut LivePeer, sequence: &mut Option<u16>) -> Result<bool, String> {
    let index = sequence.ok_or("M5: reply cadence without media")?;
    let Some(frame) = super::super::m2::codex::tone::packet(index) else {
        *sequence = None;
        return Ok(false);
    };
    peer.send_audio(frame, Instant::now().into_std())
        .await
        .map_err(|_| "M5: reply media failed")?;
    *sequence = Some(index + 1);
    Ok(true)
}
fn observation(observe: &watch::Sender<Value>, value: &Value) -> Result<(), String> {
    let (key, item) = match value["event"].as_str() {
        Some("provider.delegation") => ("delegations", value["delegation_id"].clone()),
        Some("provider.append") => ("appends", value["delegation_id"].clone()),
        Some("provider.model") => ("requests", value["request"].clone()),
        Some("provider.transcript") => ("transcripts", json!({"role":value["role"],"text":value["text"]})),
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
