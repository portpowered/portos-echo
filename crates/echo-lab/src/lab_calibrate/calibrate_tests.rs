// T1/T2 values and T3 local synthetic adb processes; no network or hardware.
use super::calibrate_options::{Clock, Deadline};
use super::*;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT: AtomicU64 = AtomicU64::new(0);
const INVENTORY: &str = "add device 1: /dev/input/event4\n  name: \"fts_ts\"\n ABS_MT_POSITION_X : min 0, max 1280\n ABS_MT_POSITION_Y : min 0, max 800\nadd device 2: /dev/input/event5\n name: \"amazon_touch\"\n ABS_MT_POSITION_X : min 0, max 1279\n ABS_MT_POSITION_Y : min 0, max 799\n";
const POINTS: [(i32, i32); 5] = [(64, 64), (1216, 64), (1216, 736), (64, 736), (640, 400)];
const PROMPTS: &str = "tap target 1 of 5: top-left at (64,64)\ntap target 2 of 5: top-right at (1216,64)\ntap target 3 of 5: bottom-right at (1216,736)\ntap target 4 of 5: bottom-left at (64,736)\ntap target 5 of 5: centre at (640,400)\n";

fn events(node: &str, points: &[(i32, i32)]) -> String {
    use std::fmt::Write;
    points.iter().fold(String::new(), |mut text, (x,y)| {
        writeln!(text, "[ 1.0] {node}: EV_KEY BTN_TOUCH DOWN\n[ 1.0] {node}: EV_ABS ABS_MT_POSITION_X {x:08x}\n[ 1.0] {node}: EV_ABS ABS_MT_POSITION_Y {y:08x}\n[ 1.0] {node}: EV_KEY BTN_TOUCH UP").unwrap();
        text
    })
}

struct Session(PathBuf);
impl Session {
    fn new(stream: &str, blocked: bool, code: u8) -> Self {
        let path = std::env::temp_dir().join(format!(
            "calibrate-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("fixture.json"),
            serde_json::to_vec(&json!({
                "shell getevent -lp":{"stdout":INVENTORY},
                "shell getevent -lt":{"stdout":stream,"block":blocked,"code":code},
                "shell getprop ro.build.version.incremental":{"stdout":"0036811211140\n"}
            }))
            .unwrap(),
        )
        .unwrap();
        Self(path)
    }

    fn fake() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_echo-lab-fake-adb"))
    }

    fn args(&self) -> Vec<String> {
        vec![
            "--adb".into(),
            Self::fake().to_str().unwrap().into(),
            "--out".into(),
            self.0.join("out").to_str().unwrap().into(),
        ]
    }

    fn configure(&self, command: &mut Command) {
        command
            .env("ECHO_LAB_FAKE_FIXTURE", self.0.join("fixture.json"))
            .env("ECHO_LAB_FAKE_LEDGER", self.0.join("ledger.jsonl"));
    }

    fn run(&self, deadline: &mut impl Deadline, output: &mut impl Write) -> Result<(), String> {
        run(
            &self.args(),
            deadline,
            |_, read| read(&["getprop", "ro.build.version.incremental"]).map(|s| s.trim().into()),
            |cmd| self.configure(cmd),
            output,
        )
    }

    fn report(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.0.join("out/calibrate.json")).unwrap()).unwrap()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct FakeDeadline {
    expire_at: usize,
}
impl Deadline for FakeDeadline {
    fn remaining(&mut self, count: usize) -> Duration {
        if count >= self.expire_at {
            Duration::ZERO
        } else {
            Duration::from_secs(10)
        }
    }
}
fn unlimited() -> FakeDeadline {
    FakeDeadline { expire_at: 6 }
}

#[test]
fn t3_handler_exact_report_log_prompts_and_read_only_ledger() {
    let text = events("/dev/input/event4", &POINTS);
    let session = Session::new(&text, false, 0);
    let mut output = Vec::new();
    assert_eq!(session.run(&mut unlimited(), &mut output), Ok(()));
    assert_eq!(
        String::from_utf8(output).unwrap(),
        format!("{PROMPTS}calibrate: glass=fts_ts max_residual_px=0\n")
    );
    assert_eq!(
        std::fs::read_to_string(session.0.join("out/getevent.log")).unwrap(),
        text
    );
    let report = session.report();
    assert_eq!(report["firmware_build"], "0036811211140");
    assert_eq!(report["glass"], "fts_ts");
    assert_eq!(
        report["fit"],
        json!({"x":{"scale":1.0,"offset":0.0,"max_residual_px":0.0},"y":{"scale":1.0,"offset":0.0,"max_residual_px":0.0},"max_residual_px":0.0})
    );
    assert_eq!(
        report["devices"],
        json!([
            {"node":"/dev/input/event4","name":"fts_ts","x":{"min":0,"max":1280},"y":{"min":0,"max":800}},
            {"node":"/dev/input/event5","name":"amazon_touch","x":{"min":0,"max":1279},"y":{"min":0,"max":799}}
        ])
    );
    let targets = ["top-left", "top-right", "bottom-right", "bottom-left", "centre"];
    let expected: Vec<_> = POINTS.iter().zip(targets).map(|((x,y), name)| json!({"device":"/dev/input/event4","raw_x":x,"raw_y":y,"target":{"name":name,"x":x,"y":y}})).collect();
    assert_eq!(report["taps"], json!(expected));
    let ledger: Vec<Value> = std::fs::read_to_string(session.0.join("ledger.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(
        ledger,
        vec![
            json!(["shell", "getprop", "ro.build.version.incremental"]),
            json!(["shell", "getevent", "-lp"]),
            json!(["shell", "getevent", "-lt"])
        ]
    );
    writeln!(std::io::stderr(), "handler report: {report}").unwrap();
}

#[test]
fn t3_fake_deadline_stops_blocked_stream_and_retains_partial_report() {
    for count in [0, 3] {
        let session = Session::new(&events("/dev/input/event4", &POINTS[..count]), true, 0);
        let error = session
            .run(&mut FakeDeadline { expire_at: count }, &mut Vec::new())
            .unwrap_err();
        assert_eq!(error, format!("calibrate: timed out after {count} of 5 taps"));
        // For zero taps the deadline expires during the first property read.
        if count != 0 {
            let report = session.report();
            assert_eq!(report["taps"].as_array().unwrap().len(), count);
            assert_eq!(report["glass"], "none");
            assert_eq!(report["fit"], Value::Null);
        }
    }
}

#[test]
fn t3_eof_malformed_and_multiple_devices_preserve_no_passing_fit() {
    let cases = [
        (
            events("/dev/input/event4", &POINTS[..3]),
            "calibrate: event stream ended before five taps",
            3,
            "none",
        ),
        (
            "[ 1.0] /dev/input/event4: EV_KEY BTN_TOUCH DOWN\n[ 1.0] /dev/input/event4: EV_KEY BTN_TOUCH UP\n".into(),
            "calibrate: tap has no position",
            0,
            "none",
        ),
        (
            format!(
                "{}{}",
                events("/dev/input/event5", &POINTS[..1]),
                events("/dev/input/event4", &POINTS)
            ),
            "calibrate: taps came from more than one device",
            6,
            "ambiguous",
        ),
    ];
    for (text, error, count, glass) in cases {
        let session = Session::new(&text, false, 0);
        assert_eq!(session.run(&mut unlimited(), &mut Vec::new()).unwrap_err(), error);
        let report = session.report();
        assert_eq!(report["taps"].as_array().unwrap().len(), count);
        assert_eq!(report["glass"], glass);
        assert_eq!(report["fit"], Value::Null);
    }
}

#[test]
fn t3_residual_failure_retains_fit_and_gate_refusal_prevents_adb() {
    let mut points = POINTS;
    points[4].0 = 680;
    let session = Session::new(&events("/dev/input/event4", &points), false, 0);
    assert_eq!(
        session.run(&mut unlimited(), &mut Vec::new()).unwrap_err(),
        "calibrate: max residual exceeds 24 px"
    );
    assert!(session.report()["fit"]["max_residual_px"].as_f64().unwrap() > 24.0);
    let refused = Session::new("", false, 0);
    assert_eq!(
        run(
            &refused.args(),
            &mut unlimited(),
            |_, _| Err("fixture gate refusal".into()),
            |cmd| refused.configure(cmd),
            &mut Vec::new()
        )
        .unwrap_err(),
        "fixture gate refusal"
    );
    assert!(!refused.0.join("ledger.jsonl").exists());
    assert!(!refused.0.join("out/calibrate.json").exists());
}

#[test]
fn arguments_and_clock_preserve_explicit_selection_and_positive_timeout() {
    let args = [
        "--out",
        "fresh",
        "--serial",
        "fixture-selector",
        "--timeout-secs",
        "7",
        "--adb",
        "explicit-adb",
    ]
    .map(str::to_owned);
    let options = Options::parse(&args).unwrap();
    assert_eq!(options.timeout, Duration::from_secs(7));
    assert_eq!(options.serial.as_deref(), Some("fixture-selector"));
    assert_eq!(options.adb, PathBuf::from("explicit-adb"));
    for args in [
        vec![],
        vec!["--adb"],
        vec!["--adb", "x", "--out", "y", "--timeout-secs", "0"],
        vec!["--adb", "x", "--adb", "z", "--out", "y"],
        vec!["--adb", "x", "--out", ""],
        vec!["--adb", "x", "--out", "y", "--unknown", "z"],
    ] {
        assert!(Options::parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
    }
    assert_eq!(Clock::new(Duration::ZERO).remaining(0), Duration::ZERO);
    assert!(Clock::new(Duration::from_secs(120)).remaining(0) <= Duration::from_secs(120));
}

#[test]
fn t3_stream_drop_reaps_and_joins_after_a_readiness_signal() {
    let session = Session::new("ready\n", true, 0);
    let mut command = probe_command::command(&Session::fake(), None, &["getevent", "-lt"]);
    session.configure(&mut command);
    command.env("ECHO_LAB_FAKE_PID", session.0.join("child.pid"));
    let stream = Stream::start(&mut command).unwrap();
    assert_eq!(
        stream.next(Duration::from_secs(10), 0).unwrap(),
        Some(b"ready\n".to_vec())
    );
    let id: u32 = std::fs::read_to_string(session.0.join("child.pid")).unwrap().parse().unwrap();
    drop(stream);
    // A reaped child cannot be waited on again by another process. The platform
    // check also proves this particular child is gone, without a timing sleep.
    if cfg!(windows) {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {id}"), "/NH"])
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&format!(" {id} ")));
    } else {
        assert!(
            !Command::new("kill")
                .args(["-0", &id.to_string()])
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
fn t3_zero_tap_deadline_expires_during_capture_and_publishes_no_fit() {
    use std::cell::Cell;
    struct CaptureDeadline<'a>(&'a Cell<bool>);
    impl Deadline for CaptureDeadline<'_> {
        fn remaining(&mut self, _: usize) -> Duration {
            if self.0.get() {
                Duration::ZERO
            } else {
                Duration::from_secs(10)
            }
        }
    }
    let capturing = Cell::new(false);
    let session = Session::new("", true, 0);
    let error = run(
        &session.args(),
        &mut CaptureDeadline(&capturing),
        |_, read| read(&["getprop", "ro.build.version.incremental"]),
        |command| {
            capturing.set(command.get_args().any(|arg| arg == "-lt"));
            session.configure(command);
        },
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(error, "calibrate: timed out after 0 of 5 taps");
    let report = session.report();
    assert_eq!(report["taps"], json!([]));
    assert_eq!(report["glass"], "none");
    assert_eq!(report["fit"], Value::Null);
    assert_eq!(std::fs::read(session.0.join("out/getevent.log")).unwrap(), b"");
}

struct Output {
    bytes: Vec<u8>,
    flushes: usize,
    fail_summary: bool,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.fail_summary && self.flushes == 5 {
            return Err(std::io::Error::other("fixture sink failure"));
        }
        self.bytes.extend(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn t3_prompts_flush_and_summary_failure_retains_completed_report() {
    for fail_summary in [false, true] {
        let session = Session::new(&events("/dev/input/event4", &POINTS), false, 0);
        let mut output = Output {
            bytes: Vec::new(),
            flushes: 0,
            fail_summary,
        };
        let result = session.run(&mut unlimited(), &mut output);
        assert_eq!(output.flushes, 5);
        assert_eq!(&output.bytes[..PROMPTS.len()], PROMPTS.as_bytes());
        if fail_summary {
            assert_eq!(result.unwrap_err(), "calibrate: stdout write failed");
        } else {
            assert_eq!(result, Ok(()));
        }
        assert_eq!(session.report()["glass"], "fts_ts");
    }
}

#[test]
fn t3_serial_selection_is_consistent_and_preexisting_output_invokes_nothing() {
    let session = Session::new(&events("/dev/input/event4", &POINTS), false, 0);
    let mut args = session.args();
    args.extend(["--serial".into(), "fixture-selector".into()]);
    assert_eq!(
        run(
            &args,
            &mut unlimited(),
            |_, read| read(&["getprop", "ro.build.version.incremental"]),
            |command| session.configure(command),
            &mut Vec::new()
        ),
        Ok(())
    );
    let ledger = std::fs::read_to_string(session.0.join("ledger.jsonl")).unwrap();
    assert_eq!(
        ledger
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>(),
        vec![
            json!([
                "-s",
                "fixture-selector",
                "shell",
                "getprop",
                "ro.build.version.incremental"
            ]),
            json!(["-s", "fixture-selector", "shell", "getevent", "-lp"]),
            json!(["-s", "fixture-selector", "shell", "getevent", "-lt"])
        ]
    );
    assert_eq!(
        session.run(&mut unlimited(), &mut Vec::new()).unwrap_err(),
        "calibrate: output destination already exists"
    );
    assert_eq!(std::fs::read_to_string(session.0.join("ledger.jsonl")).unwrap(), ledger);
    assert_eq!(session.report()["glass"], "fts_ts");
}

#[test]
fn t3_report_io_and_adb_launch_failures_cannot_claim_success() {
    let session = Session::new(&events("/dev/input/event4", &POINTS), false, 0);
    let error = run(
        &session.args(),
        &mut unlimited(),
        |_, read| read(&["getprop", "ro.build.version.incremental"]),
        |command| {
            if command.get_args().any(|arg| arg == "-lt") {
                std::fs::create_dir(session.0.join("out/calibrate.pending")).unwrap();
            }
            session.configure(command);
        },
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(error, "calibrate: report write failed");
    assert!(!session.0.join("out/calibrate.json").exists());
    let absent = Session::new("", false, 0);
    let mut args = absent.args();
    args[1] = absent.0.join("missing-adb").to_str().unwrap().into();
    assert_eq!(
        run(
            &args,
            &mut unlimited(),
            |_, read| read(&["getprop", "ro.build.version.incremental"]),
            |_| {},
            &mut Vec::new()
        )
        .unwrap_err(),
        "calibrate: cannot launch explicit adb"
    );
    assert!(!absent.0.join("out/calibrate.json").exists());
}
