use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub const FAKE: &str = env!("CARGO_BIN_EXE_echo-lab-fake-run");

pub struct Session(pub PathBuf);

impl Session {
    pub fn new(fixture: &Value) -> Self {
        let root = std::env::temp_dir().join(format!(
            "lab run test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // The name carries this process id; one that already exists is a stale leftover of an
        // interrupted earlier process that had the same id (Windows reuses ids), never a live one.
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        fs::write(root.join("fixture.json"), fixture.to_string()).unwrap();
        let session = Self(root.canonicalize().unwrap());
        session.scenario("one.yaml", &json!([{"say":"utterance.wav"}]));
        fs::write(session.0.join("utterance.wav"), b"synthetic input fixture").unwrap();
        session
    }

    pub fn scenario(&self, name: &str, steps: &Value) {
        fs::write(
            self.0.join(name),
            json!({"id":"fixture", "milestone":"B0", "tiers":["T3"],
        "setup":{}, "steps":steps, "budgets":{}, "negative":[]})
            .to_string(),
        )
        .unwrap();
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_echo-lab"));
        command
            .current_dir(&self.0)
            .env("ECHO_LAB_RING", "fixture-ring")
            .env("ECHO_LAB_RUN_FIXTURE", self.0.join("fixture.json"))
            .env("ECHO_LAB_RUN_LEDGER", self.0.join("ledger.jsonl"));
        command
    }

    pub fn run_command(&self, extra: &[&str]) -> Command {
        let mut command = self.command();
        command
            .args([
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
                FAKE,
                "--python",
                FAKE,
                "--out",
                "artifacts",
            ])
            .args(extra);
        command
    }

    pub fn run(&self, extra: &[&str]) -> Output {
        self.run_command(extra).output().unwrap()
    }

    pub fn ledger(&self) -> Vec<Value> {
        fs::read_to_string(self.0.join("ledger.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }

    pub fn operations(&self) -> Vec<String> {
        self.ledger()
            .iter()
            .map(|v| v["operation"].as_str().unwrap().to_owned())
            .collect()
    }

    pub fn report(&self) -> Value {
        let report: Value =
            serde_json::from_slice(&fs::read(self.0.join("artifacts").join("report.json")).unwrap()).unwrap();
        let schema: Value = serde_json::from_str(include_str!("../../../../schemas/report.schema.json")).unwrap();
        assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&report));
        report
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn failed(output: &Output, diagnostic: &str) {
    assert_eq!(output.status.code(), Some(1));
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(errors.contains(diagnostic), "{errors}");
    assert!(!errors.contains("fixture-private-child-output"));
}

// T3: delivered CLI, per-session fake tools and a virtual wall-clock ledger.
#[test]
fn b0_driver_fixes_file_frame_speech_waits_before_every_held_command() {
    for end in [
        "2026-11-15T09:00:00.500Z",
        "2026-11-15T10:00:00.500+01:00", // lint-rules: allow-literal synthetic timestamp parsing fixture, never a network target
        "2026-11-15T09:00:00Z",
        "2026-11-15T08:59:59Z",
    ] {
        let session = Session::new(&json!({"play":{"held":true,"playEndedAt":end}}));
        session.scenario(
            "one.yaml",
            &json!([{"say":"utterance.wav"},
            {"expect_frame":"frame.jpg"}, {"expect_speech_contains":["time"]}]),
        );
        let output = session.run(&[]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            session
                .operations()
                .into_iter()
                .filter(|op| op != "firmware")
                .collect::<Vec<_>>(),
            [
                "digest",
                "clock",
                "hold",
                "clock",
                "play",
                "wait_until",
                "snapshot",
                "clock",
                "record",
                "wav",
                "stt",
                "release"
            ]
        );
        assert_b0_ledger(&session, end);
    }
}

fn assert_b0_ledger(session: &Session, end: &str) {
    let all = session.ledger();
    let properties: Vec<_> = all
        .iter()
        .filter(|v| v["operation"] == "firmware")
        .map(|v| v["args"].clone())
        .collect();
    let expected: Vec<_> = (0..3)
        .flat_map(|_| ["ro.build.version.incremental", "ro.bootmode", "sys.boot_completed"])
        .map(|name| json!(["shell", "getprop", name]))
        .collect();
    assert_eq!(properties, expected);
    // A pending record may enter its child ledger after the next adb process.
    let ledger: Vec<_> = all.iter().filter(|v| v["operation"] != "firmware").cloned().collect();
    assert_eq!(
        ledger[4]["args"],
        json!([
            "message",
            "play-audio",
            "fixture-ring",
            "--lease",
            "fixture-lease",
            "--file",
            session.0.join("utterance.wav"),
            "--json"
        ])
    );
    assert_eq!(ledger[5]["args"], json!(["-c", "<host-helper>", "wait_until", end]));
    let reached = if end.contains(".500") {
        1_794_733_200_500_i64
    } else {
        1_794_733_200_000_i64
    };
    assert_eq!(ledger[4]["wall_ms"], 1_794_733_200_000_i64);
    for i in [6, 8, 11] {
        assert_eq!(ledger[i]["wall_ms"], reached);
    }
    assert_capture_commands(session, &ledger);
    assert!(!serde_json::to_string(&ledger).unwrap().contains("--chirp"));
    assert_eq!(
        session.report()["checks"],
        json!({"step_0":true,"step_1":true,"step_2":true,"session_released":true})
    );
    assert_eq!(
        fs::read(session.0.join("artifacts/frame.jpg")).unwrap(),
        include_bytes!("../../../../tests/goldens/lab-frame.jpg")
    );
    print_ledger(session, all);
}

fn assert_capture_commands(session: &Session, ledger: &[Value]) {
    assert_eq!(
        ledger[6]["args"],
        json!([
            "message",
            "snapshot",
            "fixture-ring",
            "--lease",
            "fixture-lease",
            "--output",
            session.0.join("artifacts/frame.jpg"),
            "--json"
        ])
    );
    assert_eq!(
        ledger[8]["args"],
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
    assert_eq!(
        ledger[11]["args"],
        json!(["session", "release", "fixture-hold", "--json"])
    );
}

fn print_ledger(session: &Session, ledger: Vec<Value>) {
    use std::io::Write;
    let mut normalized = ledger;
    for entry in &mut normalized {
        for arg in entry["args"].as_array_mut().unwrap() {
            if let Some(value) = arg.as_str() {
                *arg = Value::String(value.replace(session.0.to_str().unwrap(), "<session>"));
            }
        }
    }
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::to_string_pretty(&normalized).unwrap()
    )
    .unwrap();
}

#[test]
fn b0_driver_fixes_android_admission_has_exact_diagnostics_and_properties() {
    for (fixture, message) in [
        (
            json!({"bootmode":"recovery"}),
            "Echo is not booted to Android (bootmode recovery)",
        ),
        (
            json!({"boot_completed":""}),
            "Echo is not booted to Android (bootmode normal)",
        ),
        (
            json!({"build":"unlisted"}),
            "build is not in probed-builds.txt; coordinator probe required",
        ),
    ] {
        let session = Session::new(&fixture);
        let output = session.run(&[]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!("echo-lab: firmware: {message}\n")
        );
        assert!(!session.operations().contains(&"hold".into()));
    }
    let session = Session::new(&json!({"bootmode":"normal","boot_completed":"1"}));
    assert_eq!(session.run(&[]).status.code(), Some(0));
    let ledger = session.ledger();
    for (i, property) in ["ro.build.version.incremental", "ro.bootmode", "sys.boot_completed"]
        .iter()
        .enumerate()
    {
        assert_eq!(ledger[i + 1]["args"], json!(["shell", "getprop", property]));
    }
}

#[test]
fn b0_driver_fixes_invalid_playback_or_wait_never_dispatches_early() {
    for fixture in [
        json!({"play":{"held":true}}),
        json!({"play":{"held":false}}),
        json!({"play":{"held":true,"playEndedAt":"invalid"}}),
        json!({"play":{"held":true,"playEndedAt":"2026-11-15T09:00:00"}}), // lint-rules: allow-literal synthetic timestamp parsing fixture, never a network target
        json!({"fail":"wait_until"}),
        json!({"malformed":"wait_until"}),
        json!({"wait_until":{"reached":false}}),
    ] {
        let session = Session::new(&fixture);
        session.scenario(
            "one.yaml",
            &json!([{"say":"utterance.wav"},{"expect_frame":"frame.jpg"}]),
        );
        let output = session.run(&[]);
        assert_eq!(output.status.code(), Some(1), "{fixture}");
        let ops = session.operations();
        for op in ["snapshot", "record", "release", "stt"] {
            assert_eq!(ops.iter().filter(|v| *v == op).count(), 0, "{fixture}: {op}");
        }
        assert_eq!(ops.iter().filter(|v| *v == "play").count(), 1);
        assert_eq!(session.report()["checks"]["session_released"], false);
    }
}

#[test]
fn b0_driver_fixes_cancel_during_wait_reaps_child_and_cleanup_uses_gate() {
    let session = Session::new(&json!({"cancel_on":"wait_until","cancel_once":true,"fail":"wait_until"}));
    session.scenario(
        "one.yaml",
        &json!([{"say":"utterance.wav"},{"expect_frame":"frame.jpg"}]),
    );
    let output = session.run(&["--cancel-file", "cancel.signal"]);
    failed(&output, "child reaped");
    assert_eq!(session.report()["checks"]["session_released"], false);
    let ops = session.operations();
    assert_eq!(ops.iter().filter(|v| *v == "wait_until").count(), 2);
    for op in ["snapshot", "record", "release"] {
        assert!(!ops.contains(&op.into()));
    }
}
