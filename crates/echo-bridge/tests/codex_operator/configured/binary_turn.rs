//! T3: prebuilt CLI, real loopback media, paired saves and a held cleanup edge.
use super::restart::{binary_home, binary_ready, binary_snapshot, process_identity};
use super::*;
use echo_live::{AudioFrame, LivePeer};
use tokio::{
    net::{TcpListener, TcpStream},
    time::Instant,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Finish {
    Replace,
    Eof,
    Kill,
}
type ImageIdentity = (PathBuf, Vec<u8>, PathBuf);

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_settings_binary_active_saves_exec_once_after_played_and_cleanup() {
    exercise(Finish::Replace).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_settings_binary_eof_during_cleanup_prevents_exec() {
    exercise(Finish::Eof).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_settings_binary_kill_during_cleanup_prevents_exec() {
    exercise(Finish::Kill).await;
}

async fn exercise(finish: Finish) {
    let fixture = Fixture::new("chatgpt");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    configure(&fixture, listener.local_addr().unwrap().port());
    let mut command = super::super::voice_entry::command(&fixture);
    command.args([
        "--live-loopback",
        "http://127.0.0.1:1",
        "--model-loopback",
        "http://127.0.0.1:1",
    ]);
    let mut process = Process::spawn(&mut command);
    let pid = process.child.id().unwrap();
    let identity = process_identity(pid);
    let mut satellite = binary_ready(&listener, &mut process).await;
    let control = fixture.control().await;
    wake(&mut satellite).await;
    let mut signal = BufReader::new(control);
    let media = remote(&mut signal).await;
    held_playback(&fixture, &mut satellite).await;
    bridge_short::send(&mut satellite, "played", json!({})).await;
    let mut line = String::new();
    assert_ne!(signal.read_line(&mut line).await.unwrap(), 0);
    assert_eq!(line, "closing\n");
    cleanup_control(&fixture, &mut process, finish);
    signal.get_mut().write_all(b"close\n").await.unwrap();
    drop(signal);
    media.abort();
    finish_generation(&fixture, &listener, &mut process, finish, (pid, identity)).await;
    assert_eq!(process.stderr().await, "");
    super::super::host::released(&fixture).await;
    completed(&fixture, finish);
}

fn cleanup_control(fixture: &Fixture, process: &mut Process, finish: Finish) {
    // The child has received realtime/stop, but its reply and all later runtime
    // cleanup remain held. These controls must survive runtime destruction.
    match finish {
        Finish::Eof => drop(process.input.take()),
        Finish::Kill => std::fs::write(fixture.directory.path().join("logs/audit/bridge-kill"), b"").unwrap(),
        Finish::Replace => (),
    }
}

async fn finish_generation(
    fixture: &Fixture,
    listener: &TcpListener,
    process: &mut Process,
    finish: Finish,
    (pid, identity): (u32, ImageIdentity),
) {
    if finish == Finish::Eof {
        process.collect(0).await;
    } else {
        let replacement = binary_ready(listener, process).await;
        assert_eq!(process_identity(pid), identity);
        after_cleanup(fixture, finish).await;
        drop(process.input.take());
        process.collect(0).await;
        drop(replacement);
    }
}

fn configure(fixture: &Fixture, port: u16) {
    binary_home(fixture, port);
    let path = fixture.directory.path().join("bridge.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    // The public TOML selector is a bool or group list, not the decoded
    // AgentTools struct used by component fixtures.
    std::fs::write(path, format!("{text}\nagent_tools=false\n")).unwrap();
    std::fs::write(fixture.home.join("mode"), "tool-binary-reply").unwrap();
}

async fn wake(satellite: &mut TcpStream) {
    let row: Value = serde_json::from_str(
        include_str!("../../../../../tests/fixtures/wyoming/wake-word.jsonl")
            .lines()
            .nth(1)
            .unwrap(),
    )
    .unwrap();
    bridge_short::send(satellite, "run-pipeline", row["header"]["data"].clone()).await;
    bridge_short::send(satellite, "audio-start", json!({"rate":16000,"width":2,"channels":1})).await;
    bridge_short::send(satellite, "audio-stop", json!({})).await;
}

async fn remote(signal: &mut BufReader<TcpStream>) -> tokio::task::JoinHandle<()> {
    let mut line = String::new();
    assert_ne!(signal.read_line(&mut line).await.unwrap(), 0);
    let offer: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(offer["type"], "webrtc");
    let mut peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let answer = peer.answer(offer["sdp"].as_str().unwrap()).await.unwrap();
    signal
        .get_mut()
        .write_all(format!("{}\n", json!({"answer":answer})).as_bytes())
        .await
        .unwrap();
    while !peer.ready() {
        step(&mut peer).await;
    }
    send_audio(&mut peer).await;
    signal.get_mut().write_all(b"reply\n").await.unwrap();
    tokio::spawn(async move {
        loop {
            step(&mut peer).await;
        }
    })
}

async fn send_audio(peer: &mut LivePeer) {
    let mut encoder = echo_audio::OpusEncoder::new().unwrap();
    let mut bytes = [0; 1275];
    for sequence in 0..12_u16 {
        let size = encoder.encode(&[5000; 960], &mut bytes).unwrap();
        peer.send_audio(
            AudioFrame {
                sequence,
                timestamp: u32::from(sequence) * 960,
                payload: bytes[..size].to_vec(),
            },
            Instant::now().into_std(),
        )
        .await
        .unwrap();
    }
    peer.flush().await.unwrap();
}

async fn step(peer: &mut LivePeer) {
    let deadline = peer.flush().await.unwrap();
    tokio::select! {
        result = peer.readable() => { result.unwrap(); peer.receive(Instant::now().into_std()).await.unwrap(); },
        () = tokio::time::sleep_until(deadline.into()) => peer.timeout(Instant::now().into_std()).await.unwrap(),
    }
}

async fn held_playback(fixture: &Fixture, satellite: &mut TcpStream) {
    receive_playback(satellite).await;
    for patch in [
        json!({"agent":{"max_tool_calls":3}}),
        json!({"codex":{"voice":"marin"}}),
    ] {
        assert_eq!(
            settings_runtime_patch(fixture, patch).await,
            json!({"version":1,"restart_pending":true})
        );
    }
    let snapshot = binary_snapshot(fixture).await;
    assert_eq!(snapshot["restart_pending"], true);
    assert_eq!(snapshot["settings"]["agent"]["max_tool_calls"], 3);
    assert_eq!(snapshot["settings"]["codex"]["voice"], "marin");
    let rows = trace(&fixture.directory.path().join("trace"));
    assert_eq!(
        rows.iter()
            .filter(|row| row["event"] == "settings.restart_prepared")
            .count(),
        0
    );
    assert_eq!(
        std::fs::read_to_string(fixture.home.join("launches")).unwrap(),
        "started\n"
    );
}

async fn receive_playback(satellite: &mut TcpStream) {
    assert_eq!(bridge_short::read_reply(satellite).await.0["type"], "audio-start");
    let mut bytes = 0;
    loop {
        let (header, payload) = bridge_short::read_reply(satellite).await;
        let kind = header["type"].as_str().unwrap();
        assert!(
            matches!(kind, "audio-chunk" | "audio-stop"),
            "unexpected playback {kind}"
        );
        if kind == "audio-stop" {
            break;
        }
        bytes += payload.len();
    }
    assert!(bytes >= 320 * 2, "at least one decoded PCM frame must play");
}

async fn after_cleanup(fixture: &Fixture, finish: Finish) {
    let snapshot = binary_snapshot(fixture).await;
    assert_eq!(snapshot["restart_pending"], finish == Finish::Kill);
    assert_eq!(snapshot["settings"]["agent"]["max_tool_calls"], 3);
    assert_eq!(snapshot["settings"]["codex"]["voice"], "marin");
    if finish == Finish::Kill {
        let mut screen = bridge_wire_screen::stream(&fixture.screen.to_string()).await;
        assert_eq!(
            bridge_wire_screen::state(&mut screen).await,
            json!({
                "state":"error", "relink":false,
                "message":"Kill switch enabled. Run echo-bridge kill --clear with your config."
            })
        );
    }
}

fn completed(fixture: &Fixture, finish: Finish) {
    let rows = trace(&fixture.directory.path().join("trace"));
    let count = |event| rows.iter().filter(|row| row["event"] == event).count();
    if finish == Finish::Eof {
        // EOF may be observed by the async owner or the final synchronous gate.
        assert!(count("settings.restart_prepared") <= 1);
    } else {
        assert_eq!(count("settings.restart_prepared"), 1);
    }
    assert_eq!(count("settings.restart_failed"), 0);
    assert_eq!(count("bridge.ready"), if finish == Finish::Eof { 1 } else { 2 });
    let prepared = rows.iter().position(|row| row["event"] == "settings.restart_prepared");
    let played = rows.iter().position(|row| row["event"] == "reply.played").unwrap();
    assert!(prepared.is_none_or(|prepared| played < prepared));
    let launches = std::fs::read_to_string(fixture.home.join("launches")).unwrap();
    assert_eq!(
        launches,
        if finish == Finish::Eof {
            "started\n"
        } else {
            "started\nstarted\n"
        }
    );
}
