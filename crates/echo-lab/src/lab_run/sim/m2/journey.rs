use super::super::{controlled, world::World};
use super::{DEADLINE, auth, oracle};
use crate::lab_run::run_options::Options;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex, atomic::Ordering},
};

async fn wait(world: &mut World, event: &str) -> Result<(), String> {
    loop {
        world.refresh().await?;
        if world.trace.records.iter().any(|record| record["event"] == event) {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}

struct Case {
    world: World,
    auth: Option<auth::Auth>,
    collector: Option<tokio::task::JoinHandle<Result<(), String>>>,
    frames: Arc<Mutex<Vec<Value>>>,
    observer: Option<echo_live::fake::FakeObserver>,
    boundary: Option<usize>,
}

impl Case {
    fn new() -> Self {
        Self {
            world: World::default(),
            auth: None,
            collector: None,
            frames: Arc::new(Mutex::new(vec![])),
            observer: None,
            boundary: None,
        }
    }

    async fn start(&mut self, options: &Options, root: &Path, case: &str) -> Result<(), String> {
        std::fs::create_dir(root).map_err(|_| "m2: cannot own case directory")?;
        self.auth = Some(auth::Auth::start().await?);
        self.world.endpoints.push(
            self.auth
                .as_ref()
                .ok_or("m2: missing auth")?
                .origin
                .trim_start_matches("http://")
                .parse()
                .map_err(|_| "m2: invalid auth address")?,
        );
        self.world.m2 = Some((
            case.into(),
            self.auth.as_ref().ok_or("m2: missing auth")?.origin.clone(),
        ));
        self.world
            .start(
                &options.python,
                options.bridge.as_deref().ok_or("m2: missing bridge")?,
                root,
            )
            .await?;
        self.observer = Some(self.world.live_observer()?);
        let mut screen = self.world.screen.take().ok_or("m2: missing screen")?;
        self.frames
            .lock()
            .map_err(|_| "m2: frame lock")?
            .push(screen.current()?);
        let received = self.frames.clone();
        self.collector = Some(tokio::spawn(async move {
            loop {
                match screen.next().await {
                    Ok(frame) => received.lock().map_err(|_| "m2: frame lock")?.push(frame),
                    Err(error) if error == "screen: stream ended" => return Ok::<(), String>(()),
                    Err(error) => return Err(error),
                }
            }
        }));
        Ok(())
    }

    async fn exercise(&mut self, case: &str, utterance: &Path) -> Result<(), String> {
        let pcm = super::super::observers::bytes(utterance, 4096, "m2 utterance")?;
        self.world.control(&json!({"action":"say","pcm":pcm})).await?;
        if case == "success" {
            self.world.complete_run().await?;
            let bridge = self.world.bridge.as_mut().ok_or("m2: missing child")?;
            bridge.send(&json!({"action":"advance_clock","milliseconds":1})).await?;
            bridge.expect("bridge.clock_advanced").await?;
        } else {
            wait(&mut self.world, "auth.relink_required").await?;
            self.boundary = Some(
                self.observer
                    .as_ref()
                    .ok_or("m2: missing provider")?
                    .calls()
                    .map_err(|_| "m2: provider snapshot")?
                    .len(),
            );
            self.world.control(&json!({"action":"wake"})).await?;
            wait(&mut self.world, "run.refused").await?;
        }
        Ok(())
    }
}

pub async fn run(options: &Options, root: &Path, case: &str, utterance: &Path) -> (Value, Result<(), String>) {
    if case == "success"
        && options
            .bridge
            .as_deref()
            .is_some_and(super::super::production::selected)
    {
        return super::codex::run(options, root, utterance).await;
    }
    let mut state = Case::new();
    let root = root.join(format!("private-{case}"));
    let work = controlled(
        async {
            state.start(options, &root, case).await?;
            state.exercise(case, utterance).await
        },
        options.cancel.as_deref(),
        tokio::time::Instant::now() + DEADLINE,
    )
    .await;
    #[cfg(test)]
    let closed = super::tests::close_world(&mut state.world).await;
    #[cfg(not(test))]
    let closed = state.world.close().await;
    let mut errors = closed.err().into_iter().collect::<Vec<_>>();
    errors.extend(collect(state.collector.take()).await.err());
    let refreshes = state.auth.as_ref().map(|a| a.refreshes.load(Ordering::SeqCst));
    if let Some(auth) = state.auth.take() {
        #[cfg(test)]
        super::tests::close_auth(auth).await;
        #[cfg(not(test))]
        auth.close().await;
    }
    let measured = measurement(
        case,
        &state.world,
        &state.frames,
        state.observer.as_ref(),
        state.boundary,
        refreshes,
    );
    if root.exists() && std::fs::remove_dir(&root).is_err() {
        errors.push("m2: case directory cleanup".into());
    }
    let checked = work.and_then(|()| oracle::check(&measured));
    errors.splice(0..0, checked.err());
    let result = if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    };
    (measured, result)
}

async fn collect(task: Option<tokio::task::JoinHandle<Result<(), String>>>) -> Result<(), String> {
    let Some(mut task) = task else {
        return Ok(());
    };
    if let Ok(result) = tokio::time::timeout(DEADLINE, &mut task).await {
        result.map_err(|_| "m2: screen task failed")?
    } else {
        task.abort();
        let _ = task.await;
        Err("m2: screen teardown timed out".into())
    }
}

fn measurement(
    case: &str,
    world: &World,
    frames: &Mutex<Vec<Value>>,
    observer: Option<&echo_live::fake::FakeObserver>,
    boundary: Option<usize>,
    refreshes: Option<usize>,
) -> Value {
    let calls = observer.and_then(|o| o.calls().ok());
    let trace: Vec<_> = world
        .trace
        .records
        .iter()
        .filter(|r| {
            matches!(
                r["event"].as_str(),
                Some("live.closed" | "auth.relink_required" | "run.refused")
            )
        })
        .map(|r| json!({"event":r["event"],"reason":r["reason"],"run_id":r["run_id"]}))
        .collect();
    let wire = calls.as_ref().map(|calls| {
        calls.iter().all(|call| {
            call.request.path() == "/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas"
                && call.request.session()["model"] == "gpt-live-1-codex"
                && call.request.has_header("authorization")
                && call.request.header_matches("chatgpt-account-id", "synthetic-local")
                && call.request.header_matches("OpenAI-Alpha", "quicksilver=v2")
        })
    });
    json!({"case":case,"calls":calls.as_ref().map(Vec::len),"provider_wire":wire,"refreshes":refreshes,
        "calls_after_detection":boundary.zip(calls.as_ref()).map(|(n, calls)| calls.len().saturating_sub(n)),
        "endpoints":world.endpoints.iter().map(ToString::to_string).collect::<Vec<_>>(),"frames":frames.lock().ok().map(|f| f.clone()),"trace":trace})
}
