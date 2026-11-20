//! Paired SSE, real provider/PCM handback, and the Wyoming mute latch.
use super::*;

#[tokio::test]
#[ignore = "T3 requires prebuilt production bridge and offline Codex pin"]
async fn t3_m4_configured_screen_held_thinking_playback_and_mute() {
    let (audible, muted) = tokio::join!(exercise(false), exercise(true));
    assert_eq!(audible, Ok(()));
    assert_eq!(muted, Ok(()));
}

async fn exercise(muted: bool) -> Result<(), String> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (wake, awakened) = oneshot::channel();
    let (mute, latched) = oneshot::channel();
    let (unmute, unlatched) = oneshot::channel();
    let phases = phase::Phase::new("held delegation and playback", phase::STARTUP);
    let mut peer = tokio::spawn(satellite(
        listener,
        awakened,
        muted.then_some((latched, unlatched)),
        phases.clone(),
    ));
    let mut case = Case {
        hold_model: true,
        ..Case::default()
    };
    let observed = phases
        .wait(async {
            case.connect(&options(), false, address).await?;
            let root = &case.root.as_ref().unwrap().0;
            let mut screen = Screen::connect(root).await?;
            observe(&mut screen, &mut case.ui_states, "ready").await?;
            wake.send(()).unwrap();
            let latch = muted.then_some(mute);
            let result = work(&mut case, &mut screen, latch, &phases).await?;
            assert_eq!(result, json!({"event":"bridge.run_completed","run_id":"run-1"}));
            if muted {
                unmute.send(()).unwrap();
                observe(&mut screen, &mut case.ui_states, "ready").await?;
                advance(&mut case, 0).await?;
            }
            case.screen = Some(screen);
            Ok(())
        })
        .await;
    phases.next("held journey cleanup", phase::CLEANUP);
    let closed = phases.wait(close(&mut case, muted && observed.is_ok())).await;
    let result = phase::finish(observed, closed);
    if result.is_err() {
        peer.abort();
    }
    let bytes = phases
        .wait(async { (&mut peer).await.map_err(|_| "held Wyoming peer did not finish".into()) })
        .await;
    peer.abort();
    result.map_err(|error| case.failure(&error))?;
    assert_eq!(bytes?, 9600);
    assert_observations(&case, muted);
    Ok(())
}

async fn work(
    case: &mut Case,
    screen: &mut Screen,
    mute: Option<oneshot::Sender<()>>,
    phases: &phase::Phase,
) -> Result<Value, String> {
    let (connected, ready) = oneshot::channel();
    let provider = case.provider.as_mut().unwrap();
    let commands = provider.commands.clone();
    let mut observed = provider.observed.clone();
    let relay = case.relay.as_ref().unwrap();
    let root = &case.root.as_ref().unwrap().0;
    let states = &mut case.ui_states;
    let presentation = async {
        observe(screen, states, "listening").await?;
        observe(screen, states, "connected").await?;
        connected.send(()).unwrap();
        observe(screen, states, "thinking").await?;
        let held = observed
            .wait_for(|snapshot| snapshot["model_held"] == true)
            .await
            .map_err(|_| "held provider exited")?
            .clone();
        assert_eq!(held["delegations"], json!(["delegation-local"]));
        assert_eq!(held["appends"], json!([]));
        assert_eq!(relay.timing().played, 0);
        assert_eq!(Screen::connect(root).await?.next().await?, states[3]);
        phases.seen("model held, paired Thinking, zero PCM");
        let muted = mute.is_some();
        if let Some(mute) = mute {
            mute.send(()).unwrap();
            observe(screen, states, "muted").await?;
        }
        commands.send(Control::Reply).await.map_err(|_| "model gate closed")?;
        if !muted {
            observe(screen, states, "speaking").await?;
            observe(screen, states, "connected").await?;
            observe(screen, states, "ready").await?;
        }
        Ok::<_, String>(())
    };
    let (completed, ()) = tokio::try_join!(
        settle(case.bridge.as_mut().unwrap(), provider, relay, ready, &mut case.clock),
        presentation,
    )?;
    Ok(completed)
}

async fn satellite(
    listener: TcpListener,
    wake: oneshot::Receiver<()>,
    mute: Option<(oneshot::Receiver<()>, oneshot::Receiver<()>)>,
    phases: phase::Phase,
) -> usize {
    let mut socket = wake_satellite(listener, wake, &phases).await;
    let bytes = if let Some((latch, unlatch)) = mute {
        latch.await.unwrap();
        send(&mut socket, "hassmic-mute", json!({"muted":true}), vec![]).await;
        let format = json!({"rate":16000,"width":2,"channels":1});
        send(&mut socket, "audio-chunk", format, vec![55; 640]).await;
        let bytes = playback(&mut socket, &phases).await;
        unlatch.await.unwrap();
        send(&mut socket, "hassmic-mute", json!({"muted":false}), vec![]).await;
        bytes
    } else {
        playback(&mut socket, &phases).await
    };
    assert_eq!(socket.read(&mut [0]).await.unwrap(), 0);
    bytes
}

async fn advance(case: &mut Case, milliseconds: u64) -> Result<(), String> {
    let result =
        crate::lab_run::sim::m2::codex::collect_playout(case.bridge.as_mut().unwrap(), &mut case.clock, milliseconds)
            .await?;
    assert_eq!(result, None);
    Ok(())
}

async fn close(case: &mut Case, active_mute_observed: bool) -> Result<(), String> {
    let result = case.close().await;
    if active_mute_observed {
        // Configured shutdown retains the Control outcome of an active mute.
        // Only accept that exact exit after the run and Ready were both observed.
        assert_eq!(result, Err("cleanup: owned child exited nonzero".into()));
        assert_eq!(
            case.bridge_exit.as_deref(),
            Some("exit status: 1; forced=false; diagnostic=echo-bridge: control")
        );
        return Ok(());
    }
    result
}

fn assert_observations(case: &Case, muted: bool) {
    let journey = if muted {
        let (terminal, journey) = case.ui_states.split_last().unwrap();
        assert_eq!(
            terminal,
            &json!({"seq":7,"state":{
                "state":"error","message":"Conversation unavailable. Try another wake.","relink":false
            }})
        );
        journey
    } else {
        &case.ui_states
    };
    assert_eq!(receipts::screen(journey, &case.clock), Ok(()));
    let ready = json!({"state":"ready","message":"Echo Live ready"});
    let frames: Vec<_> = journey.iter().map(|frame| frame["state"].clone()).collect();
    let mut expected = vec![
        ready.clone(),
        json!({"state":"listening"}),
        json!({"state":"connected"}),
        json!({"state":"thinking"}),
    ];
    if muted {
        expected.push(json!({"state":"muted"}));
    } else {
        expected.extend([json!({"state":"speaking"}), json!({"state":"connected"})]);
    }
    expected.push(ready);
    assert_eq!(frames, expected);
    assert_eq!(case.transcript.as_deref(), Some(text(false)));
    stages(&case.trace);
    if muted {
        assert_eq!(case.released, ["provider", "spoof", "relay"]);
        let windows: Vec<_> = case
            .trace
            .records
            .iter()
            .filter(|row| row["event"] == "mic.mute_window")
            .collect();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0]["window_id"], "mute-1");
        assert_eq!(windows[0]["received_frames"], 1);
        assert_eq!(windows[0]["forwarded_frames"], 0);
    } else {
        assert_eq!(case.released, ["bridge", "provider", "spoof", "relay"]);
        assert_playout(&case.clock);
    }
}

fn assert_playout(clock: &Value) {
    let mut tail = 0;
    let audible_start = clock["pcm"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|frame| {
            let start = tail.max(frame["epoch_ms"].as_u64().unwrap());
            tail = start + 20;
            (frame["audible_samples"].as_u64().unwrap() > 0).then_some(start)
        })
        .unwrap();
    let thinking = clock["states"][3]["epoch_ms"].as_u64().unwrap();
    let speaking = clock["states"][4]["epoch_ms"].as_u64().unwrap();
    assert!(thinking <= audible_start);
    assert!(
        (audible_start + 200..=audible_start + 220).contains(&speaking),
        "{clock}"
    );
    writeln!(
        std::io::stdout(),
        "Held Thinking at {thinking} ms, first audible PCM at {audible_start} ms, Speaking at {speaking} ms"
    )
    .unwrap();
}
