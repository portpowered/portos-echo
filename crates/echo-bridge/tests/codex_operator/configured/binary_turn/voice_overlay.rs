//! Observe the saved overlay at the prebuilt bridge's RPC and trace boundaries.
use super::*;

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_voice_overlay_listed_is_sent_and_traced() {
    exercise_voice("haetsal", Some(&["cove", "haetsal"]), Some("haetsal"), Some(true)).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_voice_overlay_unlisted_is_omitted_and_traced() {
    exercise_voice("haetsal", Some(&["cove", "maple"]), None, Some(false)).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_voice_overlay_without_catalog_is_sent_with_unknown_membership() {
    exercise_voice("haetsal", None, Some("haetsal"), None).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_voice_overlay_default_omits_without_a_drop() {
    exercise_voice("default", Some(&["cove", "haetsal"]), None, Some(false)).await;
}

async fn exercise_voice(overlay: &str, catalog: Option<&[&str]>, sent: Option<&str>, listed: Option<bool>) {
    let fixture = Fixture::new("chatgpt");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(&fixture, listener.local_addr().unwrap().port());
    let home = fixture.directory.path().join("agent-home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("settings.toml"),
        format!("[codex]\nvoice = '{overlay}'\n[voice]\nsession_listen_enabled = false\n"),
    )
    .unwrap();
    std::fs::write(fixture.home.join("voice-catalog.json"), json!(catalog).to_string()).unwrap();
    let mut process = binary_process(&fixture);
    tokio::time::timeout(
        OBSERVATION_TIMEOUT,
        complete_voice_run(&fixture, &listener, &mut process),
    )
    .await
    .unwrap();
    let params: Value = serde_json::from_slice(&std::fs::read(fixture.home.join("realtime-config")).unwrap()).unwrap();
    assert_eq!(params.get("voice").cloned(), sent.map(Value::from));
    let dropped = overlay != "default" && listed == Some(false);
    let warning = if dropped {
        "echo-bridge: codex.voice is not listed by the app server; using its default voice\n"
    } else {
        ""
    };
    assert_eq!(process.stderr().await, warning);
    check_voice_trace(&fixture, overlay, sent, listed, dropped);
}

async fn complete_voice_run(fixture: &Fixture, listener: &TcpListener, process: &mut Process) {
    let mut satellite = binary_ready(listener, process).await;
    let mut signal = BufReader::new(fixture.control().await);
    wake(&mut satellite).await;
    let media = remote(&mut signal).await;
    receive_playback(&mut satellite, &mut signal).await;
    bridge_short::send(&mut satellite, "played", json!({})).await;
    finish_run(fixture, process, Finish::Eof, signal).await;
    process.collect(0).await;
    media.abort();
    super::super::super::host::released(fixture).await;
}

fn check_voice_trace(fixture: &Fixture, overlay: &str, sent: Option<&str>, listed: Option<bool>, dropped: bool) {
    let contents = std::fs::read_to_string(fixture.directory.path().join("trace")).unwrap();
    let rows: Vec<_> = contents
        .lines()
        .map(|line| (line, serde_json::from_str::<Value>(line).unwrap()))
        .filter(|(_, row)| {
            matches!(
                row["event"].as_str(),
                Some("call.voice_dropped" | "call.realtime_start_sent")
            )
        })
        .collect();
    assert_eq!(rows.len(), 1 + usize::from(dropped));
    if dropped {
        let drop_row = &rows[0].1;
        assert_eq!(drop_row["event"], "call.voice_dropped");
        assert_eq!(drop_row["configured_voice"], overlay);
        assert_eq!(drop_row["listed_count"], 2);
    }
    let (line, start) = &rows[usize::from(dropped)];
    assert_eq!(start["event"], "call.realtime_start_sent");
    assert_eq!(start["voice"], sent.unwrap_or("omitted"));
    assert_eq!(start.get("voice_listed"), Some(&json!(listed)));
    if listed == Some(true) {
        writeln!(std::io::stdout(), "{line}").unwrap();
    }
}
