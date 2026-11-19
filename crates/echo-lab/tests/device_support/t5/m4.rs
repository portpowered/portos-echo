//! T3: authored M4 via delivered CLI, fake bridge/bench and real spoof listener.
use super::*;
use std::{
    io::Write,
    net::{Ipv4Addr, TcpListener, TcpStream},
};

fn prepare_m4(extra: Value) -> (Session, std::thread::JoinHandle<()>, std::net::SocketAddr) {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let nominated = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let spoof = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = spoof.local_addr().unwrap();
    let mut fixture = extra;
    fixture["m4"] = true.into();
    fixture["session_replies"] = 1.into();
    fixture["transcript"] = "the test echo result".into();
    fixture["wire_port"] = upstream.local_addr().unwrap().port().into();
    let missing = fixture["missing_endpoint"] == true;
    let session = prepare(&fixture);
    let path = session.0.join("bridge.toml");
    let contents = fs::read_to_string(&path)
        .unwrap()
        .replace(
            "wyoming_port = 16700",
            &format!("wyoming_port = {}", nominated.local_addr().unwrap().port()),
        )
        .replace("http://127.0.0.1:18080/mcp", &format!("http://{address}/mcp")); // lint-rules: allow-literal runtime address reserved on Ipv4Addr::LOCALHOST
    fs::write(path, contents).unwrap();
    drop((nominated, spoof));
    let root = session.0.clone();
    let peer = std::thread::spawn(move || {
        let mut wire = session::accept(&upstream);
        session::wait_play(&root, 1);
        session::send(wire.get_mut(), "run-pipeline");
        capture(wire.get_mut(), missing);
        while session::receive(&mut wire) != "audio-stop" {}
        session::send(wire.get_mut(), "played");
        while !session::receive(&mut wire).is_empty() {}
    });
    (session, peer, address)
}

fn capture(socket: &mut TcpStream, missing: bool) {
    use echo_contracts::wyoming::{Event, Header, encode};
    // Independent input capture with changed gain; never taken from bridge trace.
    let payload = std::iter::successors(Some(0x1234_5678_u32), |state| {
        let first = state ^ (state << 13);
        let second = first ^ (first >> 17);
        Some(second ^ (second << 5))
    })
    .skip(1)
    .take(1600)
    .flat_map(|state| {
        (if missing {
            0_i16
        } else if state & 1 == 0 {
            700_i16
        } else {
            -700_i16
        })
        .to_le_bytes()
    })
    .collect::<Vec<_>>();
    socket
        .write_all(&encode(&Event {
            header: Header {
                r#type: "audio-chunk".into(),
                data: Some(json!({"rate":16000,"width":2,"channels":1})),
                data_length: 0,
                payload_length: payload.len(),
            },
            data: None,
            payload,
        }))
        .unwrap();
}

fn m4_command(session: &Session, real: bool) -> Command {
    let path = scenario().parent().unwrap().join("m4-spoof-tool.yaml");
    let mut command = voice::budget_command(session, real, &path);
    // Synthetic Port consumes authored text without making a speech service call.
    command.args(["--speech-endpoint", "http://127.0.0.1:1", "--speech-model", "synthetic"]);
    command
}

#[test]
fn t3_device_t5_m4_authored_scenario_measures_reply_and_skips_negative() {
    for real in [false, true] {
        let (session, peer, address) = prepare_m4(json!({}));
        let original = fs::read(session.0.join("bridge.toml")).unwrap();
        let output = m4_command(&session, real).output().unwrap();
        peer.join().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(session.report()["result"], "pass");
        assert_eq!(session.report()["checks"]["device_m4_latency"], true);
        assert_eq!(fs::read(session.0.join("bridge.toml")).unwrap(), original);
        let rows = observations(&session);
        let budget = rows.iter().find(|row| row["event"] == "device.budget").unwrap();
        assert_eq!(budget["limit"], 1500.0);
        assert!(budget["measured"].as_f64().unwrap() <= 1500.0);
        assert_eq!(budget["unit"], "ms");
        let negatives: Vec<_> = rows.iter().filter(|row| row["event"] == "device.negative").collect();
        assert_eq!(negatives.len(), 1);
        assert_eq!(negatives[0]["disposition"], "not run at T5 (T3/T4 cover it)");
        assert_eq!(
            rows.iter()
                .filter(|row| row["event"] == "device.ui" && row["state"]["state"] == "speaking")
                .count(),
            1
        );
        assert_cleanup(&session, address);
        let play = session
            .ledger()
            .into_iter()
            .find(|row| row["operation"] == "play")
            .unwrap();
        assert!(
            play["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == "Computer, call the test tool and tell me its result")
        );
    }
}

fn assert_cleanup(session: &Session, address: std::net::SocketAddr) {
    assert!(TcpStream::connect(address).is_err());
    assert_eq!(session.operations().iter().filter(|op| *op == "release").count(), 1);
    assert!(!session.0.join("artifacts/configured.private").exists());
    let rows = observations(session);
    let spoof = rows.iter().find(|row| row["event"] == "device.spoof").unwrap();
    assert_eq!(spoof["test_echo_calls"], 1);
    assert_eq!(spoof["negative_injected"], false);
    assert_eq!(
        session
            .ledger()
            .iter()
            .filter(|row| row["args"][0] == "forward" && row["args"][1] == "--remove")
            .count(),
        1
    );
}

#[test]
fn t3_device_t5_m4_requires_explicit_speech_before_effects() {
    let session = prepare(&json!({}));
    let path = scenario().parent().unwrap().join("m4-spoof-tool.yaml");
    let output = voice::budget_command(&session, false, &path).output().unwrap();
    failed(
        &output,
        "device: M4 utterance requires --wake-clip or --speech-endpoint and --speech-model",
    );
    assert_eq!(session.operations(), Vec::<String>::new());
    assert!(!session.0.join("artifacts").exists());
}

#[test]
fn t3_device_t5_m4_missing_endpoint_over_budget_and_late_tool_fail_closed() {
    for (fixture, error) in [
        (
            json!({"missing_endpoint":true}),
            "device: unmet prerequisite: matched finite M4 utterance endpoint",
        ),
        (
            json!({"reply_delay_ms":1600}),
            "device: first_audio_after_endpoint_ms_p95 observed ",
        ),
        (json!({"late_tool":true}), "expected 1 occurrences, observed 2"),
    ] {
        let (session, peer, address) = prepare_m4(fixture);
        let output = m4_command(&session, false).output().unwrap();
        peer.join().unwrap();
        failed(&output, error);
        if error.ends_with("observed ") {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let measured: f64 = stderr
                .split(error)
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert!(measured >= 1600.0, "{stderr}");
            assert!(stderr.contains("ms exceeds 1500 ms"), "{stderr}");
        }
        assert_eq!(session.report()["result"], "fail");
        assert_cleanup(&session, address);
    }
}
