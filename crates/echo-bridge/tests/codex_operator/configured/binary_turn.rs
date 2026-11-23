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

const READY_LINE: &str = r#"{"event":"bridge.ready"}"#;
// A process image reports its two absent prompt files once, before its first
// runtime generation; a generation the same image starts later reports none.
const IMAGE: &str = "prompt.defaulted";
const STARTED: &str = "bridge.started";
const READY: &str = "bridge.ready";
const KILL: &str = "kill.enabled";
const PREPARED: &str = "settings.restart_prepared";
const ATTEMPT: &str = "settings.reexec_attempt";
const LIFECYCLE: [&str; 9] = [
    IMAGE,
    STARTED,
    READY,
    "reply.played",
    "bridge.run_completed",
    KILL,
    PREPARED,
    ATTEMPT,
    "settings.reexec_failed",
];

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_settings_binary_active_saves_exec_once_after_played_and_cleanup() {
    exercise(Finish::Replace, false).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_settings_binary_eof_during_cleanup_prevents_exec() {
    exercise(Finish::Eof, false).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_settings_binary_kill_during_cleanup_prevents_exec() {
    exercise(Finish::Kill, false).await;
}

#[tokio::test]
#[ignore = "T3 requires prebuilt Linux echo-bridge and local Python"]
async fn t3_settings_binary_followup_off_settles_delegation_before_one_exec() {
    exercise(Finish::Replace, true).await;
}

async fn exercise(finish: Finish, delegated: bool) {
    let fixture = Fixture::new("chatgpt");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut process = launch(&fixture, listener.local_addr().unwrap().port(), delegated);
    let pid = process.child.id().unwrap();
    let mut satellite = binary_ready(&listener, &mut process).await;
    let identity = process_identity(pid);
    let address = screen_address(&fixture.directory.path().join("trace"), 0).await;
    let control = fixture.control().await;
    wake(&mut satellite).await;
    let mut signal = BufReader::new(control);
    let media = remote(&mut signal).await;
    held_playback(&fixture, address, &mut satellite, &mut signal).await;
    bridge_short::send(&mut satellite, "played", json!({})).await;
    complete_delegation(&fixture, address, &mut signal, delegated).await;
    finish_run(&fixture, &mut process, finish, signal).await;
    media.abort();
    finish_generation(&fixture, &listener, &mut process, finish, (pid, identity)).await;
    settled(&fixture, &mut process, finish).await;
}

async fn finish_run(fixture: &Fixture, process: &mut Process, finish: Finish, mut signal: BufReader<TcpStream>) {
    closing(&mut signal).await;
    assert_eq!(
        process.line().await,
        r#"{"event":"bridge.run_completed","run_id":"run-1"}"#
    );
    cleanup_control(fixture, process, finish);
    signal.get_mut().write_all(b"close\n").await.unwrap();
}

fn launch(fixture: &Fixture, port: u16, delegated: bool) -> Process {
    configure(fixture, port);
    configure_delegation(fixture, delegated);
    let mut command = super::super::voice_entry::command(fixture);
    Process::spawn(command.args([
        "--live-loopback",
        "http://127.0.0.1:1",
        "--model-loopback",
        "http://127.0.0.1:1",
    ]))
}

async fn complete_delegation(
    fixture: &Fixture,
    address: SocketAddr,
    signal: &mut BufReader<TcpStream>,
    delegated: bool,
) {
    if !delegated {
        return;
    }
    let snapshot = binary_snapshot(address).await;
    assert_eq!(snapshot["restart_pending"], true);
    let rows = trace(&fixture.directory.path().join("trace"));
    for event in [PREPARED, ATTEMPT, "bridge.run_completed"] {
        assert_eq!(rows.iter().filter(|row| row["event"] == event).count(), 0);
    }
    signal.get_mut().write_all(b"settle\n").await.unwrap();
}

async fn settled(fixture: &Fixture, process: &mut Process, finish: Finish) {
    let output = remaining_output(process).await;
    assert_eq!(process.stderr().await, "");
    super::super::host::released(fixture).await;
    completed(fixture, finish, &output);
}

/// Every stdout line the exited process published and no step consumed.
async fn remaining_output(process: &mut Process) -> Vec<String> {
    let mut lines = vec![];
    while let Some(line) = tokio::time::timeout(OBSERVATION_TIMEOUT, process.lines.next_line())
        .await
        .unwrap()
        .unwrap()
    {
        lines.push(line);
    }
    lines
}

async fn closing(signal: &mut BufReader<TcpStream>) {
    let mut line = String::new();
    assert_ne!(signal.read_line(&mut line).await.unwrap(), 0);
    assert_eq!(line, "closing\n");
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
        // Readiness is service state, not a generation identity: the first
        // generation returns to its wait and reports it once more, and only
        // that wait takes the pending restart. The replacement's own report
        // follows its fresh device admission.
        assert_eq!(process.line().await, READY_LINE);
        let replacement = binary_ready(listener, process).await;
        assert_eq!(process_identity(pid), identity);
        let address = screen_address(&fixture.directory.path().join("trace"), 1).await;
        after_cleanup(address, finish).await;
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

async fn held_playback(
    fixture: &Fixture,
    address: SocketAddr,
    satellite: &mut TcpStream,
    signal: &mut BufReader<TcpStream>,
) {
    receive_playback(satellite, signal).await;
    for patch in [
        json!({"agent":{"daily_request_cap":"3"}}),
        json!({"codex":{"voice":"maple"}}),
    ] {
        assert_eq!(
            settings_runtime_patch(address, patch).await,
            json!({"version":1,"restart_pending":true})
        );
    }
    let snapshot = binary_snapshot(address).await;
    assert_eq!(snapshot["restart_pending"], true);
    assert_eq!(snapshot["settings"]["agent"]["daily_request_cap"], "3");
    assert_eq!(snapshot["settings"]["codex"]["voice"], "maple");
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

async fn receive_playback(satellite: &mut TcpStream, signal: &mut BufReader<TcpStream>) {
    assert_eq!(playback_frame(satellite).await.0["type"], "audio-start");
    let mut bytes = 0;
    loop {
        let (header, payload) = playback_frame(satellite).await;
        let kind = header["type"].as_str().unwrap();
        assert!(
            matches!(kind, "audio-chunk" | "audio-stop"),
            "unexpected playback {kind}"
        );
        if kind == "audio-stop" {
            break;
        }
        // UDP media and the RPC pipe have independent ordering. Complete the
        // synthetic reply only after real decoded audio crosses Wyoming.
        if bytes == 0 && !payload.is_empty() {
            signal.get_mut().write_all(b"reply\n").await.unwrap();
        }
        bytes += payload.len();
    }
    assert!(bytes >= 320 * 2, "at least one decoded PCM frame must play");
}

async fn playback_frame(satellite: &mut TcpStream) -> (Value, Vec<u8>) {
    loop {
        let (header, payload) = bridge_short::read_reply(satellite).await;
        if header["type"] != "hassmic-thinking" {
            return (header, payload);
        }
        // Delegation may update the display during playback. Consume only the
        // exact public display envelope; all other control frames still fail.
        assert!(header["data"] == json!({"thinking":true}) || header["data"] == json!({"thinking":false}));
        assert_eq!(payload, Vec::<u8>::new());
    }
}

async fn after_cleanup(address: SocketAddr, finish: Finish) {
    let snapshot = binary_snapshot(address).await;
    assert_eq!(snapshot["restart_pending"], finish == Finish::Kill);
    assert_eq!(snapshot["settings"]["agent"]["daily_request_cap"], "3");
    assert_eq!(snapshot["settings"]["codex"]["voice"], "maple");
    if finish == Finish::Kill {
        let mut screen = bridge_wire_screen::stream(&address.to_string()).await;
        assert_eq!(
            bridge_wire_screen::state(&mut screen).await,
            json!({
                "state":"error", "relink":false,
                "message":"Kill switch enabled. Run echo-bridge kill --clear with your config."
            })
        );
    }
}

fn completed(fixture: &Fixture, finish: Finish, output: &[String]) {
    let rows = trace(&fixture.directory.path().join("trace"));
    let count = |event| rows.iter().filter(|row| row["event"] == event).count();
    if finish == Finish::Eof {
        // EOF may be observed by the async owner or the final synchronous gate.
        assert!(count("settings.restart_prepared") <= 1);
    } else {
        assert_eq!(count("settings.restart_prepared"), 1);
    }
    assert_eq!(count("settings.reexec_failed"), 0);
    assert_eq!(count(ATTEMPT), usize::from(finish == Finish::Replace));
    assert_eq!(count("settings.saved"), 2);
    assert_eq!(count("settings.reexec_pending"), 1);
    assert!(
        rows.iter()
            .filter(|row| row["event"] == "settings.saved")
            .all(|row| row["restart_pending"] == true)
    );
    generations(&rows, finish);
    // Each readiness row has exactly one stdout line: three were consumed
    // around a replacement, one before an EOF.
    let consumed = if finish == Finish::Eof { 1 } else { 3 };
    assert_eq!(output, vec![READY_LINE; count(READY) - consumed]);
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

/// The exact lifecycle order across process images and runtime generations.
fn generations(rows: &[Value], finish: Finish) {
    let lifecycle: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["event"].as_str())
        .filter(|event| LIFECYCLE.contains(event))
        .collect();
    let turn = [IMAGE, IMAGE, STARTED, READY, "reply.played", "bridge.run_completed"];
    assert!(lifecycle.starts_with(&turn), "{lifecycle:?}");
    let after = &lifecycle[turn.len()..];
    match finish {
        // One real exec: a second image starts the only other generation.
        Finish::Replace => assert_eq!(after, [READY, PREPARED, ATTEMPT, IMAGE, IMAGE, STARTED, READY]),
        // No exec: the same image starts the next generation, still killed.
        Finish::Kill => assert_eq!(after, [KILL, READY, PREPARED, STARTED, KILL, READY]),
        // No second image or generation. EOF may win during the held cleanup,
        // in the renewed wait, or only at the final synchronous gate.
        Finish::Eof => {
            let allowed: [&[&str]; 3] = [&[], &[READY], &[READY, PREPARED]];
            assert!(allowed.contains(&after), "{after:?}");
        }
    }
}

fn configure_delegation(fixture: &Fixture, delegated: bool) {
    if delegated {
        let base = fixture.directory.path().join("bridge.toml");
        let text = std::fs::read_to_string(&base).unwrap();
        std::fs::write(base, text.replace("[voice]", "[voice]\nsession_listen_enabled=false")).unwrap();
        std::fs::write(fixture.home.join("delegated"), "").unwrap();
    }
}
