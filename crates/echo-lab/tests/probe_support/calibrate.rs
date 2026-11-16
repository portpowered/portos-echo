// T3: prebuilt CLI and synthetic adb, isolated directories and child-only env.
use crate::probe_support::{FAKE, Session, response};
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::process::{Command, Output, Stdio};

const POINTS: [(i32, i32); 5] = [(64, 64), (1216, 64), (1216, 736), (64, 736), (640, 400)];
const PROMPTS: &str = "tap target 1 of 5: top-left at (64,64)\ntap target 2 of 5: top-right at (1216,64)\ntap target 3 of 5: bottom-right at (1216,736)\ntap target 4 of 5: bottom-left at (64,736)\ntap target 5 of 5: centre at (640,400)\n";
const INVENTORY: &str = "add device 1: /dev/input/event4\n name: \"fts_ts\"\n ABS_MT_POSITION_X : min 0, max 1280\n ABS_MT_POSITION_Y : min 0, max 800\nadd device 2: /dev/input/event5\n name: \"amazon_touch\"\n ABS_MT_POSITION_X : min 0, max 1279\n ABS_MT_POSITION_Y : min 0, max 799\n";

fn events(node: &str, points: &[(i32, i32)]) -> String {
    use std::fmt::Write;
    points.iter().fold(String::new(), |mut text, (x,y)| {
        writeln!(text, "[ 1.0] {node}: EV_KEY BTN_TOUCH DOWN\n[ 1.0] {node}: EV_ABS ABS_MT_POSITION_X {x:08x}\n[ 1.0] {node}: EV_ABS ABS_MT_POSITION_Y {y:08x}\n[ 1.0] {node}: EV_KEY BTN_TOUCH UP").unwrap();
        text
    })
}

fn fixture(stream: &str) -> Value {
    json!({
        "shell getprop ro.build.version.incremental":response("0036811211140\n"),
        "shell getprop ro.bootmode":response("normal\n"),
        "shell getprop sys.boot_completed":response("1\n"),
        "shell getevent -lp":response(INVENTORY),
        "shell getevent -lt":response(stream)
    })
}

fn command(session: &Session, serial: bool) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_echo-lab"));
    cmd.args(["calibrate", "--adb", FAKE, "--out"])
        .arg(session.0.join("out"))
        .env("ECHO_LAB_FAKE_FIXTURE", session.0.join("overrides.json"))
        .env("ECHO_LAB_FAKE_LEDGER", session.0.join("ledger.jsonl"));
    if serial {
        cmd.args(["--serial", "fixture-selector"]);
    }
    cmd
}

fn report(session: &Session) -> Value {
    serde_json::from_slice(&fs::read(session.0.join("out/calibrate.json")).unwrap()).unwrap()
}

fn expected_report() -> Value {
    let names = ["top-left", "top-right", "bottom-right", "bottom-left", "centre"];
    let taps: Vec<_> = POINTS
        .iter()
        .zip(names)
        .map(|((x, y), name)| {
            json!({
                "device":"/dev/input/event4", "raw_x":x,"raw_y":y,
                "target":{"name":name,"x":x,"y":y}
            })
        })
        .collect();
    json!({"firmware_build":"0036811211140", "glass":"fts_ts", "taps":taps,
        "devices":[
            {"node":"/dev/input/event4","name":"fts_ts","x":{"min":0,"max":1280},"y":{"min":0,"max":800}},
            {"node":"/dev/input/event5","name":"amazon_touch","x":{"min":0,"max":1279},"y":{"min":0,"max":799}}
        ],
        "fit":{"x":{"scale":1.0,"offset":0.0,"max_residual_px":0.0},"y":{"scale":1.0,"offset":0.0,"max_residual_px":0.0},"max_residual_px":0.0}
    })
}

#[test]
fn calibrate_real_cli_exact_journey_and_serial_ledger() {
    let text = events("/dev/input/event4", &POINTS);
    for serial in [false, true] {
        let session = Session::new(&fixture(&text));
        let output = command(&session, serial).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(output.stderr, b"");
        assert_eq!(
            output.stdout,
            format!("{PROMPTS}calibrate: glass=fts_ts max_residual_px=0\n").as_bytes()
        );
        assert_eq!(report(&session), expected_report());
        assert_eq!(fs::read_to_string(session.0.join("out/getevent.log")).unwrap(), text);
        let expected: Vec<Value> = [
            vec!["shell", "getprop", "ro.build.version.incremental"],
            vec!["shell", "getprop", "ro.bootmode"],
            vec!["shell", "getprop", "sys.boot_completed"],
            vec!["shell", "getevent", "-lp"],
            vec!["shell", "getevent", "-lt"],
        ]
        .into_iter()
        .map(|args| {
            if serial {
                json!([vec!["-s", "fixture-selector"], args].concat())
            } else {
                json!(args)
            }
        })
        .collect();
        assert_eq!(session.ledger(), expected);
        writeln!(
            std::io::stderr(),
            "serial={serial}\n{}calibrate.json:\n{}\nledger: {}",
            String::from_utf8_lossy(&output.stdout),
            serde_json::to_string_pretty(&report(&session)).unwrap(),
            json!(session.ledger())
        )
        .unwrap();
    }
}

fn failure(output: &Output, error: &str) {
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, format!("{error}\n").as_bytes());
}

#[test]
fn calibrate_real_cli_gate_refusals_stop_before_capture() {
    for (key, value, error, reads) in [
        (
            "ro.build.version.incremental",
            "unknown",
            "firmware: build is not in probed-builds.txt; coordinator probe required",
            1,
        ),
        (
            "ro.bootmode",
            "recovery",
            "firmware: Echo is not booted to Android (bootmode recovery)",
            3,
        ),
        (
            "sys.boot_completed",
            "0",
            "firmware: Echo is not booted to Android (bootmode normal)",
            3,
        ),
    ] {
        let mut data = fixture("");
        data[format!("shell getprop {key}")] = response(value);
        let session = Session::new(&data);
        let output = command(&session, false).output().unwrap();
        failure(&output, error);
        assert_eq!(output.stdout, b"");
        assert_eq!(session.ledger().len(), reads);
        assert!(!session.0.join("out/calibrate.json").exists());
        assert!(session.ledger().iter().all(|v| v[1] == "getprop"));
    }
}

#[test]
fn calibrate_real_cli_partial_ambiguous_and_residual_failures() {
    let mut bad = POINTS;
    bad[4].0 = 680;
    let cases = [
        (
            events("/dev/input/event4", &POINTS[..3]),
            "calibrate: event stream ended before five taps",
            "none",
            3,
        ),
        (
            format!(
                "{}{}",
                events("/dev/input/event5", &POINTS[..1]),
                events("/dev/input/event4", &POINTS)
            ),
            "calibrate: taps came from more than one device",
            "ambiguous",
            6,
        ),
        (
            events("/dev/input/event4", &bad),
            "calibrate: max residual exceeds 24 px",
            "fts_ts",
            5,
        ),
        (
            "[ 1.0] /dev/input/event4: EV_KEY BTN_TOUCH DOWN\n[ 1.0] /dev/input/event4: EV_KEY BTN_TOUCH UP\n".into(),
            "calibrate: tap has no position",
            "none",
            0,
        ),
    ];
    for (text, error, glass, taps) in cases {
        let session = Session::new(&fixture(&text));
        failure(&command(&session, false).output().unwrap(), error);
        let value = report(&session);
        assert_eq!(value["glass"], glass);
        assert_eq!(value["taps"].as_array().unwrap().len(), taps);
        if glass == "fts_ts" {
            assert!(value["fit"]["max_residual_px"].as_f64().unwrap() > 24.0);
        } else {
            assert_eq!(value["fit"], Value::Null);
        }
    }
}

#[test]
fn calibrate_cli_usage_destinations_and_race() {
    let cases = [
        (vec![], "calibrate: --adb is required"),
        (vec!["--adb", FAKE], "calibrate: --out is required"),
        (vec!["--adb"], "calibrate: flags require values"),
        (vec!["--unknown", "x"], "calibrate: invalid or duplicate argument"),
        (
            vec!["--adb", FAKE, "--adb", FAKE],
            "calibrate: invalid or duplicate argument",
        ),
        (
            vec!["--adb", FAKE, "--out", "unused", "--timeout-secs", "0"],
            "calibrate: timeout must be a positive integer",
        ),
    ];
    for (args, error) in cases {
        let session = Session::new(&fixture(""));
        let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
            .arg("calibrate")
            .args(args)
            .env("ECHO_LAB_FAKE_LEDGER", session.0.join("ledger.jsonl"))
            .output()
            .unwrap();
        failure(&output, error);
        assert_eq!(session.ledger(), Vec::<Value>::new());
    }
    let session = Session::new(&fixture(&events("/dev/input/event4", &POINTS)));
    let first = command(&session, false)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = command(&session, false).output().unwrap();
    let first = first.wait_with_output().unwrap();
    assert_eq!(
        [first.status.success(), second.status.success()]
            .iter()
            .filter(|v| **v)
            .count(),
        1
    );
    let loser = if first.status.success() { &second } else { &first };
    failure(loser, "calibrate: output destination already exists");
    assert_eq!(loser.stdout, b"");
    assert_eq!(report(&session), expected_report());
    fs::write(session.0.join("out/sentinel"), "retained").unwrap();
    let before = session.ledger();
    failure(
        &command(&session, false).output().unwrap(),
        "calibrate: output destination already exists",
    );
    assert_eq!(session.ledger(), before);
    assert_eq!(fs::read_to_string(session.0.join("out/sentinel")).unwrap(), "retained");
}

#[test]
fn calibrate_cli_dependency_inventory_and_destination_errors() {
    for (key, value, error) in [
        (
            "shell getprop ro.bootmode",
            json!({"stdout":"","code":2}),
            "calibrate: adb read failed",
        ),
        (
            "shell getevent -lp",
            response("malformed inventory"),
            "probe getevent: no named devices in nonempty observation",
        ),
    ] {
        let mut data = fixture("");
        data[key] = value;
        let session = Session::new(&data);
        let output = command(&session, false).output().unwrap();
        failure(&output, error);
        assert!(!session.0.join("out/calibrate.json").exists());
    }
    let session = Session::new(&fixture(""));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_echo-lab"));
    let output = cmd
        .args(["calibrate", "--adb"])
        .arg(session.0.join("missing-adb"))
        .arg("--out")
        .arg(session.0.join("out"))
        .output()
        .unwrap();
    failure(&output, "calibrate: cannot launch explicit adb");
    let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
        .args(["calibrate", "--adb", FAKE, "--out"])
        .arg(session.0.join("missing-parent/out"))
        .output()
        .unwrap();
    failure(&output, "calibrate: cannot create output directory");
    let mut errors = Vec::new();
    assert_eq!(crate::lab_calibrate::execute(&[], &mut Vec::new(), &mut errors), 1);
    assert_eq!(errors, b"calibrate: --adb is required\n");
}
