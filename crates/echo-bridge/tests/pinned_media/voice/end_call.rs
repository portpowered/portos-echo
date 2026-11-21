//! T3: asking to end the call reaches `end_session` through the pinned app
//! server, and the bridge applies it. On the Echo the realtime model only
//! answered a goodbye; this pins the hand-off's other half.
use super::runner_skills::{
    Output, acknowledge_cut, admit_journey, check_thread, configure_skills, declarations, drive_peer, read_json,
    read_rpc, send_json, start_provider, voice_context,
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
    let (mut satellite, stdin, mut peer) = Box::pin(admit_journey(
        root,
        &listener,
        &config,
        &mut bridge,
        &mut control,
        &mut observed,
    ))
    .await;
    observe_end_call(root, &tools, &mut peer, &mut control, &mut observed).await;
    drop(stdin);
    acknowledge_cut(&mut satellite).await;
    let output = bridge.wait_with_output().await.unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    peer.close();
    drop(control);
    assert!(provider.wait().await.unwrap().success());
}

async fn observe_end_call(
    root: &Path,
    tools: &[Value],
    peer: &mut LivePeer,
    control: &mut tokio::process::ChildStdin,
    observed: &mut Output,
) {
    // The user's request becomes a delegation; its answer is appended only
    // after the model has seen end_session's result.
    send_json(control, json!({"transcripts":true})).await;
    assert_eq!(
        drive_peer(peer, read_json(observed)).await,
        json!({"append":"end-call"})
    );
    send_json(control, json!({"session":true})).await;
    let context = voice_context(&read_json(observed).await);
    let rows = read_rpc(root);
    check_thread(&rows[0], &context, tools, false);
    send_json(control, json!({"snapshot":"end-call"})).await;
    check_end(root, &rows[1..], &read_json(observed).await);
}

fn check_end(root: &Path, rows: &[Value], provider: &Value) {
    let ending = json!({"ending_after_reply":true}).to_string();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["result"],
        json!({"contentItems":[{"type":"inputText","text":ending}],"success":true})
    );
    assert_eq!(provider["external"], 0);
    assert_eq!(provider["appends"], json!(["end-call"]));
    let outputs = provider["end_outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0]["call_id"], "end-call-end_session");
    assert_eq!(outputs[0]["output"], ending);
    let receipts: Vec<Value> = std::fs::read_to_string(root.join("trace"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["event"] == "session.tool_receipt")
        .collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0]["tool"], "end_session");
    assert_eq!(receipts[0]["outcome"], "applied");
    writeln!(
        std::io::stderr(),
        "end call: RPC {} receipt {}",
        rows[0]["result"],
        receipts[0]
    )
    .unwrap();
}
