//! T3 public CLI journeys with prebuilt tools; Python codec edges stay real.
use crate::run_support::{FAKE, Session, failed};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn b0(session: &Session, keep: bool) -> Output {
    session
        .command()
        .args(["run"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/scenarios/b0-stock-time.yaml"))
        .args([
            "--driver",
            "device",
            "--bench",
            "ring",
            "--port",
            FAKE,
            "--adb",
            FAKE,
            "--stt",
            FAKE,
            "--python",
            FAKE,
            "--out",
            "artifacts",
            "--speech-endpoint",
            "fixture-speech",
            "--speech-model",
            "fixture-model",
        ])
        .args(if keep { vec!["--keep-audio"] } else { vec![] })
        .output()
        .unwrap()
}

fn success(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn f13_f14_f20_f23_f29_b0_pass_fail_frame_and_audio_privacy() {
    for (transcript, pass, keep) in [
        ("The time is 7:49 p.m.", true, false), // lint-rules: allow-literal synthetic STT transcript for passing B0 without retained audio
        ("The time is 7:49 p.m.", true, true), // lint-rules: allow-literal synthetic STT transcript for passing B0 with retained audio
        ("No clock information", false, false),
    ] {
        let session = Session::new(&json!({"transcript":transcript,"record_waits_for_frame":true}));
        let output = b0(&session, keep);
        if pass {
            success(&output);
        } else {
            failed(&output, "speech expectation:");
        }
        let report = session.report();
        assert_eq!(report["result"], if pass { "pass" } else { "fail" });
        assert_eq!(
            report["checks"],
            json!({"step_0":true,"step_1":true,"step_2":pass,"session_released":true})
        );
        assert_eq!(report["evidence"], json!(["events.jsonl", "light-ring.jpg"]));
        assert_eq!(
            fs::read(session.0.join("artifacts/light-ring.jpg")).unwrap(),
            include_bytes!("../../../../tests/goldens/lab-frame.jpg")
        );
        for name in ["alignment.wav", "reply-0.wav"] {
            assert_eq!(session.0.join("artifacts").join(name).exists(), keep || !pass);
        }
        let operations = session.operations();
        for op in ["hold", "chirp", "play", "record", "snapshot", "stt", "release"] {
            assert_eq!(operations.iter().filter(|v| v.as_str() == op).count(), 1, "{op}");
        }
        let at = |op| operations.iter().position(|v| v == op).unwrap();
        assert!(at("play") < at("record"));
        assert!(at("snapshot") < at("wav") && at("wav") < at("stt") && at("stt") < at("release"));
        assert_capture_argv(&session);
        assert_private(&session, &output, transcript);
    }
}

fn assert_capture_argv(session: &Session) {
    let ledger = session.ledger();
    let snapshot = ledger.iter().find(|v| v["operation"] == "snapshot").unwrap();
    assert_eq!(
        snapshot["args"],
        json!([
            "message",
            "snapshot",
            "fixture-ring",
            "--lease",
            "fixture-lease",
            "--output",
            session.0.join("artifacts/light-ring.jpg"),
            "--json"
        ])
    );
    let record = ledger.iter().find(|v| v["operation"] == "record").unwrap();
    assert_eq!(
        record["args"],
        json!([
            "message",
            "record",
            "fixture-ring",
            "--lease",
            "fixture-lease",
            "--audio-only",
            "--duration",
            "10",
            "--output",
            session.0.join("artifacts/reply-0.wav"),
            "--json"
        ])
    );
}

fn assert_private(session: &Session, output: &Output, transcript: &str) {
    let mut text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for file in ["report.json", "events.jsonl"] {
        text.push_str(&fs::read_to_string(session.0.join("artifacts").join(file)).unwrap());
    }
    for private in [
        "fixture-ring",
        "fixture-lease",
        "fixture-hold",
        "fixture-private-child-output",
        transcript,
    ] {
        assert!(!text.contains(private), "persisted {private}");
    }
}

#[test]
fn f15_f17_file_speech_uses_one_trimmed_wav_argument_and_literal_phrases() {
    let session = Session::new(&json!({"transcript":"TEST Echo [a.*]"}));
    session.scenario(
        "one.yaml",
        &json!([{"say":"utterance.wav"}, {"expect_speech_contains":["test","echo","[a.*]"]}]),
    );
    success(&session.run(&["--keep-audio"]));
    let ledger = session.ledger();
    let stt = ledger.iter().find(|v| v["operation"] == "stt").unwrap();
    assert_eq!(stt["args"], json!([session.0.join("artifacts/reply-0.wav")]));
    assert_eq!(session.report()["checks"]["step_1"], true);
    let operations = session.operations();
    assert!(
        operations.iter().position(|v| v == "reply").unwrap() < operations.iter().position(|v| v == "stt").unwrap()
    );
}

#[test]
fn f16_f26_stt_absent_failure_empty_and_no_reply_fail_without_raw_output() {
    for fixture in [
        json!({"fail":"stt"}),
        json!({"transcript":""}),
        json!({"transcript":"fixture-private-child-output"}),
    ] {
        let session = Session::new(&fixture);
        session.scenario(
            "one.yaml",
            &json!([{"say":"utterance.wav"},{"expect_speech_contains":["expected"]}]),
        );
        let output = session.run(&[]);
        failed(&output, "speech expectation:");
        assert_eq!(session.report()["checks"]["step_1"], false);
        assert_private(&session, &output, "unpublished transcript");
    }
    let session = Session::new(&json!({}));
    session.scenario("one.yaml", &json!([{"expect_speech_contains":["expected"]}]));
    failed(&session.run(&[]), "no recorded reply");
    assert_eq!(session.operations(), ["digest", "firmware"]);
    let session = Session::new(&json!({}));
    session.scenario(
        "one.yaml",
        &json!([{"say":"utterance.wav"},{"expect_speech_contains":["time"]}]),
    );
    // Supply missing executable in the original argv, preserving duplicate-flag rejection.
    let mut command = session.command();
    command.args([
        "run",
        "one.yaml",
        "--driver",
        "device",
        "--bench",
        "ring",
        "--port",
        FAKE,
        "--adb",
        FAKE,
        "--stt",
        "nonexistent-stt-fixture",
        "--python",
        FAKE,
        "--out",
        "artifacts",
    ]);
    failed(&command.output().unwrap(), "STT command failed");
    assert_eq!(session.report()["result"], "fail");
}

#[test]
fn f21_frame_failures_are_never_silently_passed() {
    for (fixture, diagnostic) in [
        (json!({"fail":"snapshot"}), "frame capture:"),
        (json!({"missing_image":true}), "missing image"),
        (json!({"snapshot":{"success":false}}), "capture confirmation"),
    ] {
        let session = Session::new(&fixture);
        failed(&b0(&session, false), diagnostic);
        assert_eq!(session.report()["result"], "fail");
        assert_eq!(session.operations().last().unwrap(), "release");
        assert!(!session.operations().contains(&"stt".into()));
    }
}

#[test]
fn f21_structurally_corrupt_jpeg_fails_frame_step_and_releases_hold() {
    let session = Session::new(&json!({"snapshot_file":"broken.jpg"}));
    // Intact SOI/EOI envelope with an out-of-bounds table segment.
    fs::write(
        session.0.join("broken.jpg"),
        [0xff, 0xd8, 0xff, 0xdb, 0, 40, 0xff, 0xd9],
    )
    .unwrap();
    failed(&b0(&session, false), "frame capture: JPEG truncated segment");
    assert_eq!(session.report()["checks"]["step_1"], false);
    assert_eq!(session.report()["checks"]["session_released"], true);
    assert_eq!(session.report()["result"], "fail");
    assert_eq!(session.report()["evidence"], json!(["events.jsonl"]));
    assert_eq!(session.operations().last().unwrap(), "release");
    assert!(!session.operations().contains(&"stt".into()));
}

#[test]
fn f21_pending_record_failures_are_checked_by_oracle_and_finalization() {
    for (fixture, diagnostic) in [
        (json!({"fail":"record"}), "record:"),
        (json!({"record":{"success":true,"hasAudio":false}}), "no audio"),
        (json!({"fail":"wav"}), "invalid reply WAV"),
    ] {
        let session = Session::new(&fixture);
        failed(&b0(&session, false), diagnostic);
        assert_eq!(session.report()["result"], "fail");
        assert_eq!(session.operations().last().unwrap(), "release");
        assert!(!session.operations().contains(&"stt".into()));
    }
    let session = Session::new(&json!({"fail":"record"}));
    session.scenario(
        "one.yaml",
        &json!([{"say":"utterance.wav"},{"expect_frame":"frame.jpg"}]),
    );
    failed(&session.run(&[]), "record:");
    assert_eq!(session.report()["checks"]["session_released"], false);
}

#[test]
fn f22_f28_unsafe_paths_and_invalid_patterns_refuse_before_effects() {
    for path in [
        "../escape.png",
        "/tmp/escape.jpg",
        "C:/escape.jpg",
        "frames/../../escape.jpg",
    ] {
        let session = Session::new(&json!({}));
        session.scenario("one.yaml", &json!([{"expect_frame":path}]));
        failed(&session.run(&[]), "/steps/0/expect_frame:");
        let validation = session.command().args(["validate", "one.yaml"]).output().unwrap();
        assert_eq!(validation.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&validation.stdout).contains("/steps/0/expect_frame:"));
        assert_eq!(session.operations(), Vec::<String>::new());
    }
    for phrases in [json!([]), json!([""]), json!(["regex:"]), json!(["regex:["])] {
        let session = Session::new(&json!({}));
        session.scenario("one.yaml", &json!([{"expect_speech_contains":phrases}]));
        failed(&session.run(&[]), "speech expectation:");
        assert_eq!(session.operations(), Vec::<String>::new());
    }
}

fn png(kind: &str) -> Vec<u8> {
    let script = r"import struct,zlib,sys
def chunk(k,d): return struct.pack('>I',len(d))+k+d+struct.pack('>I',zlib.crc32(k+d))
kind=sys.argv[1]
w=3 if kind=='dimensions' else 2
raw=(bytes([0])+bytes([1 if kind=='pixel' else 0,0,0,255])*w)*2
header=struct.pack('>IIBBBBB',w,2,8,6,0,0,0)
data=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',header)+chunk(b'tEXt',b'fixture\0different encoding')+chunk(b'IDAT',zlib.compress(raw,0))+chunk(b'IEND',b'')
sys.stdout.buffer.write(b'broken' if kind=='corrupt' else data)";
    let output = Command::new("python").args(["-c", script, kind]).output().unwrap();
    success(&output);
    output.stdout
}

fn screen_session(kind: &str, fail_adb: bool) -> (Session, Vec<u8>) {
    let session = Session::new(&json!({}));
    let bytes = png(kind);
    fs::write(session.0.join("capture.png"), &bytes).unwrap();
    fs::write(
        session.0.join("fixture.json"),
        json!({"screencap_file":session.0.join("capture.png"),
        "real_pixels":true, "fail":if fail_adb { "screencap" } else { "none" }})
        .to_string(),
    )
    .unwrap();
    session.scenario(
        "one.yaml",
        &json!([{"expect_screen":{"golden":"tests/goldens/lab-screen.png","tolerance":0}}]),
    );
    (session, bytes)
}

#[test]
fn f18_equal_decoded_pixels_pass_with_different_png_encoding_and_preserve_raw_bytes() {
    let (session, bytes) = screen_session("equal", false);
    assert_ne!(bytes, include_bytes!("../../../../tests/goldens/lab-screen.png"));
    success(&session.run(&[]));
    assert_eq!(session.operations(), ["digest", "firmware", "screencap", "pixels"]);
    assert_eq!(session.ledger()[2]["args"], json!(["exec-out", "screencap", "-p"]));
    assert_eq!(fs::read(session.0.join("artifacts/screen-0.png")).unwrap(), bytes);
    assert_eq!(session.report()["evidence"], json!(["events.jsonl", "screen-0.png"]));
    assert_eq!(session.report()["checks"]["step_0"], true);
}

#[test]
fn f19_screen_mismatch_dimensions_corruption_and_adb_errors_fail() {
    for (kind, fail_adb, diagnostic) in [
        ("pixel", false, "decoded pixels differ"),
        ("dimensions", false, "dimensions differ"),
        ("corrupt", false, "PNG decode failed"),
        ("equal", true, "capture failed"),
    ] {
        let (session, _) = screen_session(kind, fail_adb);
        failed(&session.run(&[]), diagnostic);
        assert_eq!(session.report()["checks"]["step_0"], false);
    }
    let session = Session::new(&json!({}));
    session.scenario(
        "one.yaml",
        &json!([{"expect_screen":{"golden":"tests/goldens/lab-screen.png","tolerance":1}}]),
    );
    failed(&session.run(&[]), "tolerance: 0");
    assert_eq!(session.operations(), Vec::<String>::new());
}

#[test]
fn f25_parallel_runs_are_isolated_and_shared_destination_has_one_owner() {
    let a = Session::new(&json!({}));
    let b = Session::new(&json!({}));
    std::thread::scope(|scope| {
        let first = scope.spawn(|| b0(&a, false));
        success(&b0(&b, false));
        success(&first.join().unwrap());
    });
    assert_isolated_b0(&a, &b);
    assert_isolated_b0(&b, &a);
    let session = Session::new(&json!({}));
    let outputs = std::thread::scope(|scope| {
        let first = scope.spawn(|| session.run(&[]));
        (session.run(&[]), first.join().unwrap())
    });
    let mut codes = [outputs.0.status.code().unwrap(), outputs.1.status.code().unwrap()];
    codes.sort_unstable();
    assert_eq!(codes, [0, 1]);
    assert_eq!(session.operations().iter().filter(|v| *v == "hold").count(), 1);
}

fn assert_isolated_b0(session: &Session, other: &Session) {
    let report = session.report();
    assert_eq!(report["result"], "pass");
    assert_eq!(
        report["checks"],
        json!({"step_0":true,"step_1":true,"step_2":true,"session_released":true})
    );
    assert_eq!(report["evidence"], json!(["events.jsonl", "light-ring.jpg"]));
    let operations = session.operations();
    for op in ["hold", "chirp", "play", "record", "snapshot", "stt", "release"] {
        assert_eq!(operations.iter().filter(|v| v.as_str() == op).count(), 1, "{op}");
    }
    assert_capture_argv(session);
    let ledger = session.ledger();
    let stt = ledger.iter().find(|v| v["operation"] == "stt").unwrap();
    assert_eq!(stt["args"], json!([session.0.join("artifacts/reply-0.wav")]));
    assert_isolated_paths(session, other);
    assert_eq!(
        fs::read(session.0.join("artifacts/light-ring.jpg")).unwrap(),
        include_bytes!("../../../../tests/goldens/lab-frame.jpg")
    );
    for file in ["report.json", "events.jsonl"] {
        let bytes = fs::read(session.0.join("artifacts").join(file)).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains(other.0.to_str().unwrap()));
    }
}

fn assert_isolated_paths(session: &Session, other: &Session) {
    for entry in session.ledger() {
        let args = entry["args"].as_array().unwrap();
        for arg in args {
            let path = Path::new(arg.as_str().unwrap());
            assert!(!path.starts_with(&other.0), "cross-session argument: {path:?}");
        }
        for pair in args.windows(2) {
            if pair[0] == "--output" {
                let path = Path::new(pair[1].as_str().unwrap());
                assert!(
                    path.starts_with(session.0.join("artifacts")),
                    "foreign output: {path:?}"
                );
            }
        }
    }
}
