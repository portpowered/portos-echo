//! T3: asking to end the call reaches `end_session` through the pinned app
//! server, and the bridge applies it. On the Echo the realtime model only
//! answered a goodbye; this pins the hand-off's other half. The end also
//! supersedes the delegated turn still running (owner, 2026-11-21): the
//! bridge interrupts it instead of waiting for its answer.
use super::runner_skills::{
    Output, admit_journey, check_thread, configure_skills, declarations, drive_peer, read_json, read_rpc, send_json,
    start_provider, voice_context,
};
use super::*;

#[tokio::test]
async fn t3_pinned_end_call_request_applies_end_session() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(end_call_journey()))
        .await
        .unwrap();
}

async fn end_call_journey() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = configure_skills(root, listener.local_addr().unwrap().port(), None);
    let (mut provider, mut control, mut observed) = start_provider();
    let ready = read_json(&mut observed).await;
    let origin = ready["origin"].as_str().unwrap();
    let tools = declarations(false, None);
    send_json(&mut control, json!({"end_call":tools})).await;
    assert_eq!(read_json(&mut observed).await, json!({"initialized":"end-call"}));
    let mut bridge = launch(root, origin);
    let (mut satellite, stdin, mut peer, stdout) = Box::pin(admit_journey(
        root,
        &listener,
        &config,
        &mut bridge,
        &mut control,
        &mut observed,
    ))
    .await;
    observe_end_call(root, &tools, (&mut peer, &mut satellite), &mut control, &mut observed).await;
    drop(stdin);
    let output = bridge.wait_with_output().await.unwrap();
    drop(stdout);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    peer.close();
    drop(control);
    assert!(provider.wait().await.unwrap().success());
}

async fn observe_end_call(
    root: &Path,
    tools: &[Value],
    (peer, satellite): (&mut LivePeer, &mut TcpStream),
    control: &mut tokio::process::ChildStdin,
    observed: &mut Output,
) {
    // The user's request becomes a delegation. Its model never answers after
    // end_session's result: the applied end ends the run on the device and
    // interrupts the running turn, which would otherwise hold the run.
    // Read the admitted call's prompt before the end retires that call.
    send_json(control, json!({"session":true})).await;
    let context = voice_context(&read_json(observed).await);
    send_json(control, json!({"transcripts":true})).await;
    let mut wire = Vec::new();
    drive_peer(peer, async {
        tokio::join!(interrupted(root), device_until_end(satellite, &mut wire))
    })
    .await;
    writeln!(std::io::stderr(), "end call: wire {wire:?}").unwrap();
    send_json(control, json!({"release_end":true})).await;
    let released = reply(observed).await;
    let rows = read_rpc(root);
    check_thread(&rows[0], &context, tools, false);
    send_json(control, json!({"snapshot":"end-call"})).await;
    check_end(root, &rows[1..], &reply(observed).await, &released);
}

/// The next fixture reply. The bridge may retire the idle call meanwhile,
/// which closes its sideband.
async fn reply(observed: &mut Output) -> Value {
    loop {
        let value = read_json(observed).await;
        if value != json!({"sideband_closed":true}) {
            return value;
        }
    }
}

fn trace(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("trace"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

/// The barrier is the turn's own end: the bridge may stop the idle call
/// before it reads the interrupt's acknowledgement.
async fn interrupted(root: &Path) {
    while !trace(root).iter().any(|row| row["event"] == "turn.interrupted") {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The satellite acknowledges any cut playback and records each command
/// until the bridge ends the run.
async fn device_until_end(satellite: &mut TcpStream, wire: &mut Vec<String>) {
    loop {
        let (header, payload) = read_wire(satellite).await;
        assert_eq!(payload, b"");
        let kind = header["type"].as_str().unwrap().to_owned();
        if kind == "hassmic-flush" {
            send(satellite, "played", json!({}), vec![]).await;
        }
        let ended = kind == "hassmic-end";
        wire.push(kind);
        if ended {
            return;
        }
    }
}

fn check_end(root: &Path, rows: &[Value], provider: &Value, released: &Value) {
    let ending = json!({"ending_after_reply":true}).to_string();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["result"],
        json!({"contentItems":[{"type":"inputText","text":ending}],"success":true})
    );
    assert_eq!(provider["external"], 0);
    // The interrupted turn hands nothing back to the voice.
    assert_eq!(provider["appends"], json!([]));
    let outputs = provider["end_outputs"].as_array().unwrap();
    assert!(outputs.len() <= 1);
    assert!(
        released["models"]
            .as_u64()
            .is_some_and(|models| (1..=2).contains(&models))
    );
    let events = trace(root);
    let receipts: Vec<&Value> = events
        .iter()
        .filter(|row| row["event"] == "session.tool_receipt")
        .collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["tool"], "end_session");
    assert_eq!(receipts[0]["outcome"], "applied");
    // The interrupt names the turn that called end_session, and that turn
    // ends as interrupted.
    let turn = &events.iter().find(|row| row["event"] == "item/tool/call").unwrap()["turn_id"];
    let interrupts = correlated(&events, "turn/interrupt", turn);
    assert_eq!((interrupts, correlated(&events, "turn.interrupted", turn)), (1, 1));
    assert!(correlated(&events, "turn.interrupt_acknowledged", turn) <= 1);
    writeln!(
        std::io::stderr(),
        "end call: receipt {} interrupts {interrupts} models {}",
        receipts[0]["outcome"],
        released["models"]
    )
    .unwrap();
}

fn correlated(events: &[Value], event: &str, turn: &Value) -> usize {
    let rows: Vec<&Value> = events.iter().filter(|row| row["event"] == event).collect();
    assert!(rows.iter().all(|row| &row["turn_id"] == turn), "{event}");
    rows.len()
}
