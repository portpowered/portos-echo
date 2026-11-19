//! T3: authored listen sessions through the CLI, fake bridge and wire peer.
use super::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    time::{Duration, Instant},
};

fn prepare_session(replies: u64, extra: Value) -> (Session, std::thread::JoinHandle<()>) {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let nominated = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut fixture = extra;
    fixture["wire_port"] = upstream.local_addr().unwrap().port().into();
    fixture["session_replies"] = replies.into();
    let session = prepare(&fixture);
    let path = session.0.join("bridge.toml");
    let config = fs::read_to_string(&path).unwrap().replace(
        "wyoming_port = 16700",
        &format!("wyoming_port = {}", nominated.local_addr().unwrap().port()),
    );
    fs::write(path, config).unwrap();
    drop(nominated);
    let root = session.0.clone();
    let peer = std::thread::spawn(move || host(&upstream, &root, replies));
    (session, peer)
}

fn host(listener: &TcpListener, root: &Path, replies: u64) {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut wire = loop {
        if let Ok((socket, _)) = listener.accept() {
            break BufReader::new(socket);
        }
        assert!(Instant::now() < deadline, "synthetic wire never connected");
        std::thread::yield_now();
    };
    wire.get_mut().set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    for turn in 1..=replies {
        wait_play(root, turn);
        if turn == 1 {
            send(wire.get_mut(), "run-pipeline");
        }
        send(wire.get_mut(), "audio-chunk");
        while receive(&mut wire) != "audio-stop" {}
        send(wire.get_mut(), "played");
    }
    // Hold the peer open through driver finalization, including missing-end cases.
    while !receive(&mut wire).is_empty() {}
}

fn wait_play(root: &Path, count: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let played = fs::read_to_string(root.join("ledger.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|row| serde_json::from_str::<Value>(row).ok())
            .filter(|row| row["operation"] == "play")
            .count();
        if u64::try_from(played).unwrap() >= count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "next utterance waited for whole-run completion"
        );
        std::thread::yield_now();
    }
}

fn send(socket: &mut TcpStream, kind: &str) {
    use echo_contracts::wyoming::{Event, Header, encode};
    let payload = if kind == "audio-chunk" { vec![0, 4] } else { vec![] };
    socket
        .write_all(&encode(&Event {
            header: Header {
                r#type: kind.into(),
                data: Some(json!({"rate":16000,"width":2,"channels":1})),
                data_length: 0,
                payload_length: payload.len(),
            },
            data: None,
            payload,
        }))
        .unwrap();
}

fn receive(socket: &mut BufReader<TcpStream>) -> String {
    let mut header = String::new();
    if socket.read_line(&mut header).unwrap() == 0 {
        return String::new();
    }
    let header: echo_contracts::wyoming::Header = serde_json::from_str(&header).unwrap();
    socket
        .read_exact(&mut vec![0; header.data_length + header.payload_length])
        .unwrap();
    header.r#type
}

fn session_command(session: &Session, id: &str, real: bool) -> Command {
    let path = scenario().parent().unwrap().join(format!("{id}.yaml"));
    let mut command = voice::budget_command(session, real, &path);
    // The fake Port boundary receives text directly and never calls this service.
    command.args(["--speech-endpoint", "http://127.0.0.1:1", "--speech-model", "synthetic"]);
    command
}

#[test]
fn t3_device_t5_listen_sessions_execute_authored_speech_with_one_wire_run() {
    for (id, replies, real) in [("session-two-turns", 2, false), ("session-idle-timeout", 1, true)] {
        let (session, peer) = prepare_session(replies, json!({}));
        let original = fs::read(session.0.join("bridge.toml")).unwrap();
        let output = session_command(&session, id, real).output().unwrap();
        peer.join().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(session.report()["result"], "pass");
        assert_eq!(session.report()["checks"]["device_session"], true);
        assert_eq!(fs::read(session.0.join("bridge.toml")).unwrap(), original);
        let rows = observations(&session);
        let timing = rows.iter().find(|row| row["event"] == "device.session_timing").unwrap();
        assert_eq!(timing["replies"], replies);
        assert_eq!(timing["session_listen_seconds"], 3);
        assert!(timing["idle_offset_us"].as_u64().unwrap() <= 100_000);
        assert_eq!(rows.iter().filter(|row| row["kind"] == "run-pipeline").count(), 1);
        assert_plays(&session, replies);
        assert_eq!(session.operations().iter().filter(|op| *op == "release").count(), 1);
        assert!(!session.operations().iter().any(|op| op == "record"));
        assert!(!session.0.join("artifacts/configured.private").exists());
    }
}

fn assert_plays(session: &Session, replies: u64) {
    let plays: Vec<_> = session
        .ledger()
        .into_iter()
        .filter(|row| row["operation"] == "play")
        .collect();
    assert_eq!(plays.len(), usize::try_from(replies).unwrap());
    assert!(
        plays[0]["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "Computer, what time is it?")
    );
    if replies == 2 {
        assert!(
            plays[1]["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == "Computer, what day is it?")
        );
    }
}

#[test]
fn t3_device_t5_listen_sessions_missing_end_and_late_error_revoke_success() {
    for (fixture, error) in [
        (
            json!({"missing_end":true}),
            "device: unmet prerequisite: one complete wire session identity",
        ),
        (json!({"late_error":true}), "device: bridge errors observed"),
        (
            json!({"idle_ms":3105}),
            "device: session idle expected 3000 ms +/-100 ms",
        ),
    ] {
        let (session, peer) = prepare_session(1, fixture);
        let output = session_command(&session, "session-idle-timeout", false)
            .output()
            .unwrap();
        peer.join().unwrap();
        failed(&output, error);
        assert_eq!(session.report()["result"], "fail");
        assert_eq!(session.operations().iter().filter(|op| *op == "release").count(), 1);
        assert!(!session.0.join("artifacts/configured.private").exists());
    }
}
