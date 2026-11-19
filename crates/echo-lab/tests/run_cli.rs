#[cfg(test)]
#[path = "run_support/device.rs"]
mod device;
#[cfg(test)]
#[path = "run_support/lease.rs"]
mod lease;
#[cfg(test)]
#[path = "run_support/oracles.rs"]
mod oracles;
#[cfg(test)]
mod run_support;
#[cfg(test)]
#[path = "device_support/t5/mod.rs"]
mod t5;
#[cfg(test)]
mod tests {
    use crate::run_support::{FAKE, Session, failed};
    // T3 through prebuilt CLI/tools; per-child fixtures and isolated directories.
    use serde_json::{Value, json};
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn link_counts_without_measured_outcomes_fail_before_any_device_operation() {
        let session = Session::new(&json!({}));
        session.scenario(
            "one.yaml",
            &json!([{"expect_link_counts":{
            "link":0,"tools_list":0,"tool_call":0,"forced_refresh":0,
            "replay_rejected":0,"relink_error":0,"restart_grant_kept":0,"unlink":0}}]),
        );
        let output = session.run(&[]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "echo-lab: link counts: measurement unavailable\n"
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}: fail\n", session.0.join("artifacts").display())
        );
        assert_eq!(session.operations(), ["digest"]);
        let report = session.report();
        assert_eq!(report["result"], "fail");
        assert_eq!(report["checks"], json!({"step_0":false,"session_released":true}));
        assert!(report.get("link_counts").is_none());
    }

    #[test]
    fn f01_one_dispatch_hold_reply_release_and_atomic_report() {
        let session = Session::new(&json!({}));
        let output = session.run(&[]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).ends_with(": pass\n"));
        assert_eq!(
            session.operations(),
            [
                "digest",
                "firmware",
                "firmware",
                "firmware",
                "clock",
                "hold",
                "clock",
                "play",
                "reply",
                "wait_until",
                "release"
            ]
        );
        assert_ledger(&session);
        assert_artifacts(&session);
    }

    fn assert_ledger(session: &Session) {
        let ledger = session.ledger();
        assert_eq!(
            ledger[1]["args"],
            json!(["shell", "getprop", "ro.build.version.incremental"])
        );
        assert_eq!(
            ledger[5]["args"],
            json!(["session", "hold", "fixture-ring", "--camera", "--listen", "--json"])
        );
        assert_eq!(
            ledger[7]["args"],
            json!([
                "message",
                "play-audio",
                "fixture-ring",
                "--lease",
                "fixture-lease",
                "--file",
                session.0.join("utterance.wav").canonicalize().unwrap(),
                "--listen",
                "10",
                "--output",
                session.0.join("artifacts").join("reply-0.wav"),
                "--json"
            ])
        );
        assert_eq!(
            ledger[10]["args"],
            json!(["session", "release", "fixture-hold", "--json"])
        );
    }

    fn assert_artifacts(session: &Session) {
        let report = session.report();
        assert_eq!(report["result"], "pass");
        assert_eq!(report["checks"], json!({"step_0":true,"session_released":true}));
        assert_eq!(report["bench_session_ids"], json!(["batch-hold-1"]));
        assert!(!session.0.join("artifacts").join("alignment.wav").exists());
        assert!(!session.0.join("artifacts").join("reply-0.wav").exists());
        assert!(!session.0.join("artifacts").join("report.pending").exists());
        let events: Vec<Value> = fs::read_to_string(session.0.join("artifacts").join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|v| serde_json::from_str(v).unwrap())
            .collect();
        assert_eq!(
            events.iter().map(|v| v["event"].as_str().unwrap()).collect::<Vec<_>>(),
            [
                "run_started",
                "step_started",
                "hold_opened",
                "step_completed",
                "hold_closed",
                "run_completed"
            ]
        );
        assert_eq!(events[1]["step"], 0);
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0]["elapsed_ms"].as_u64() <= pair[1]["elapsed_ms"].as_u64())
        );
        let artifacts = fs::read_to_string(session.0.join("artifacts").join("report.json")).unwrap();
        assert!(!artifacts.contains("fixture-lease"));
        assert!(!artifacts.contains("fixture-ring"));
    }

    #[test]
    fn f02_invalid_schema_and_missing_utterance_have_no_external_effects() {
        for content in [
            "{}",
            "steps: [",
            r#"{"id":"fixture","milestone":"B0","tiers":["T3"],"setup":{},"steps":[{"say":"absent.wav"}],"budgets":{},"negative":[]}"#,
        ] {
            let session = Session::new(&json!({}));
            fs::write(session.0.join("one.yaml"), content).unwrap();
            failed(
                &session.run(&[]),
                if content.contains("absent") {
                    "utterance WAV is missing"
                } else {
                    "scenario:"
                },
            );
            assert_eq!(session.operations(), Vec::<String>::new());
            assert!(!session.0.join("artifacts").exists());
        }
    }

    #[test]
    fn f03_quiet_0859_refuses_and_0900_allows_audio() {
        let session = Session::new(&json!({"hour":8}));
        failed(&session.run(&[]), "before 09:00"); // lint-rules: allow-literal expected quiet-hours stderr, never a socket address
        assert_eq!(
            session.operations(),
            ["digest", "firmware", "firmware", "firmware", "clock"]
        );
        assert_eq!(session.report()["result"], "fail");
        let session = Session::new(&json!({"hour":9}));
        assert_eq!(session.run(&[]).status.code(), Some(0));
        assert_eq!(session.ledger().iter().filter(|v| v["operation"] == "play").count(), 1);
    }

    #[test]
    fn f03b_owner_waiver_plays_before_0900_and_forwards_flag_to_playback_only() {
        let session = Session::new(&json!({"hour":8}));
        assert_eq!(session.run(&["--allow-quiet-hours"]).status.code(), Some(0));
        let ledger = session.ledger();
        let waived = |op: &str| {
            ledger
                .iter()
                .filter(|v| v["operation"] == op)
                .all(|v| v["args"].as_array().unwrap().iter().any(|a| a == "--allow-quiet-hours"))
        };
        assert_eq!(ledger.iter().filter(|v| v["operation"] == "play").count(), 1);
        assert!(waived("play"));
        for op in ["hold", "release", "firmware", "clock"] {
            assert!(
                ledger.iter().filter(|v| v["operation"] == op).all(|v| !v["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|a| a == "--allow-quiet-hours")),
                "{op}"
            );
        }
        let sim = Session::new(&json!({}));
        failed(
            &sim.command()
                .args(["run", "one.yaml", "--driver", "sim", "--allow-quiet-hours"])
                .output()
                .unwrap(),
            "sim options only",
        );
    }

    #[test]
    fn f04_unprobed_firmware_and_adb_fail_before_hold() {
        for fixture in [json!({"build":"unprobed"}), json!({"fail":"firmware"})] {
            let session = Session::new(&fixture);
            failed(&session.run(&[]), "firmware:");
            assert_eq!(session.operations(), ["digest", "firmware"]);
            assert_eq!(session.report()["result"], "fail");
        }
    }

    #[test]
    fn f05_audio_failure_and_malformed_hold_release_without_child_output() {
        for (fixture, message, release) in [
            (json!({"fail":"play"}), "play:", false),
            (json!({"malformed":"play"}), "play:", false),
            (json!({"hold":{"holdId":"fixture-hold"}}), "missing lease", true),
            (
                json!({"hold":{"holdId":"fixture-hold","lease":"fixture-lease"}}),
                "listening and talking",
                true,
            ),
            (json!({"malformed":"hold"}), "hold:", false),
            (json!({"fail":"hold"}), "hold:", false),
            (json!({"fail":"reply"}), "invalid reply WAV", true),
            (json!({"reply":{"reply_frames":0}}), "no post-playback", true),
        ] {
            let session = Session::new(&fixture);
            failed(&session.run(&[]), message);
            assert_eq!(
                session.operations().iter().filter(|s| *s == "release").count(),
                usize::from(release)
            );
            assert_eq!(session.report()["result"], "fail");
        }
    }

    #[test]
    fn f06_clock_rechecked_before_every_utterance() {
        let session = Session::new(&json!({"hours":[9,8]}));
        failed(&session.run(&[]), "before 09:00"); // lint-rules: allow-literal expected pre-audio quiet-hours diagnostic
        assert_eq!(
            session.operations(),
            [
                "digest", "firmware", "firmware", "firmware", "clock", "hold", "clock", "release"
            ]
        );
    }

    #[test]
    fn f24_output_in_any_worktree_and_existing_output_are_refused_without_tools() {
        let session = Session::new(&json!({}));
        fs::create_dir(session.0.join("artifacts")).unwrap();
        failed(&session.run(&[]), "destination already exists");
        fs::remove_dir(session.0.join("artifacts")).unwrap();
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let output = session
            .command()
            .args([
                "run", "one.yaml", "--driver", "device", "--bench", "ring", "--port", FAKE, "--adb", FAKE, "--stt",
                FAKE, "--out",
            ])
            .arg(repo.join("forbidden-artifacts"))
            .output()
            .unwrap();
        failed(&output, "outside all repository worktrees");
        assert_eq!(session.operations(), Vec::<String>::new());
    }

    #[test]
    fn f27_batch_reuses_hold_and_checks_firmware_each_step() {
        let session = Session::new(&json!({}));
        session.scenario("two.yaml", &json!([{"say":"utterance.wav"}]));
        assert_eq!(session.run(&["two.yaml", "--keep-audio"]).status.code(), Some(0));
        let operations = session.operations();
        for (op, count) in [("hold", 1), ("release", 1), ("chirp", 0), ("play", 2), ("firmware", 6)] {
            assert_eq!(operations.iter().filter(|s| s.as_str() == op).count(), count, "{op}");
        }
        assert!(session.0.join("artifacts").join("reply-1.wav").is_file());
        assert!(
            !session
                .ledger()
                .iter()
                .any(|v| v["args"].as_array().unwrap().iter().any(|a| a == "--allow-quiet-hours"))
        );
    }

    #[test]
    fn text_requires_explicit_configuration_before_effects_and_passes_it_verbatim() {
        let session = Session::new(&json!({}));
        session.scenario("one.yaml", &json!([{"say":"text:synthetic utterance"}]));
        failed(&session.run(&[]), "both --speech-endpoint and --speech-model");
        assert_eq!(session.operations(), Vec::<String>::new());
        assert_eq!(
            session
                .run(&["--speech-endpoint", "fixture-speech", "--speech-model", "fixture-model"])
                .status
                .code(),
            Some(0)
        );
        let ledger = session.ledger();
        let play = ledger.iter().find(|v| v["operation"] == "play").unwrap();
        assert_eq!(
            play["args"],
            json!([
                "message",
                "play-audio",
                "fixture-ring",
                "--lease",
                "fixture-lease",
                "--text",
                "synthetic utterance",
                "--speech-endpoint",
                "fixture-speech",
                "--speech-model",
                "fixture-model",
                "--listen",
                "10",
                "--output",
                session.0.join("artifacts").join("reply-0.wav"),
                "--json"
            ])
        );
        assert!(
            !fs::read_to_string(session.0.join("artifacts").join("events.jsonl"))
                .unwrap()
                .contains("synthetic utterance")
        );
    }

    #[test]
    fn unsupported_step_and_failed_release_produce_failed_reports() {
        let session = Session::new(&json!({}));
        session.scenario("one.yaml", &json!([{"expect_trace":["fixture"]}]));
        failed(&session.run(&[]), "capability is not implemented");
        assert_eq!(session.report()["checks"]["step_0"], false);
        assert_eq!(session.operations(), ["digest", "firmware", "firmware", "firmware"]);
        let session = Session::new(&json!({"fail":"release"}));
        failed(&session.run(&[]), "release failed");
        assert_eq!(session.report()["checks"]["session_released"], false);
        assert!(session.0.join("artifacts").join("reply-0.wav").exists());
    }

    #[test]
    fn invalid_options_cannot_invoke_tools() {
        let session = Session::new(&json!({}));
        for args in [
            vec!["--python"],
            vec!["--driver", "sim"],
            vec!["--unknown", "x"],
            vec!["--keep-audio", "--keep-audio"],
            vec!["--speech-model", "only"],
        ] {
            failed(&session.run(&args), "run:");
        }
        assert_eq!(session.operations(), Vec::<String>::new());
    }

    #[test]
    fn local_real_host_helper_trims_exact_post_playback_samples_and_hashes_input() {
        let session = Session::new(&json!({}));
        assert_eq!(session.run(&["--keep-audio"]).status.code(), Some(0));
        let helper = include_str!("../src/lab_run/run_host.py");
        let wav = session.0.join("artifacts").join("reply-0.wav");
        let output = Command::new("python")
            .args(["-c", helper, "reply"])
            .arg(&wav)
            .args([
                "2026-11-15T09:00:00Z",
                "2026-11-15T09:00:00.500Z",
                "2026-11-15T09:00:00Z",
                "2026-11-15T09:00:01Z",
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            json!({"reply_frames":2})
        );
        assert_eq!(&fs::read(wav).unwrap()[44..], &[3, 0, 4, 0]);
        fs::write(session.0.join("hash-input"), b"abc").unwrap();
        let output = Command::new("python")
            .args(["-c", helper, "digest"])
            .arg(session.0.join("hash-input"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["sha256"],
            "c3494ca1a2cf8eeb8a11ded316fb55b83c3bbbedb6313cd50415251e5d09e12f"
        );
    }

    #[test]
    fn f24_publication_failure_and_broken_stdout_never_claim_success() {
        let session = Session::new(&json!({"block_report":true}));
        let output = session.run(&[]);
        failed(&output, "cannot write report");
        assert_eq!(output.stdout, b"");
        assert!(!session.0.join("artifacts").join("report.json").exists());
        assert_eq!(session.operations().last().unwrap(), "release");
        let session = Session::new(&json!({}));
        let mut child = session
            .run_command(&[])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        let output = child.wait_with_output().unwrap();
        failed(&output, "stdout: write failed");
        assert_eq!(session.report()["result"], "pass");
        assert_eq!(session.operations().last().unwrap(), "release");
    }
    #[test]
    fn local_real_host_helper_waits_on_instants_with_a_controlled_wall_clock() {
        let helper = include_str!("../src/lab_run/run_host.py");
        let target = "2026-11-15T10:00:00.250+01:00"; // lint-rules: allow-literal offset timestamp parsing fixture
        let script = format!(
            r#"
import sys,datetime
from unittest.mock import patch
ns={{"__name__":"test"}}
exec({helper:?},ns)
base=datetime.datetime(2026,11,15,9,tzinfo=datetime.timezone.utc)
state={{"now":base,"waits":[]}}
class Clock(datetime.datetime):
    @classmethod
    def now(cls,tz=None): return state["now"]
def advance(seconds):
    state["waits"].append(seconds)
    state["now"] += datetime.timedelta(seconds=seconds)
with patch.object(ns["datetime"],"datetime",Clock), patch.object(ns["time"],"sleep",advance):
    assert ns["wait_until"]({target:?})=={{"reached":True}}
    assert state["now"]==base+datetime.timedelta(seconds=.25)
    assert state["waits"]==[.1,.1,.05],state
    state["waits"].clear()
    assert ns["wait_until"]("2026-11-15T09:00:00.250Z")=={{"reached":True}}
    assert state["waits"]==[]
    assert ns["wait_until"]("2026-11-15T09:00:00.250000001Z")=={{"reached":True}}
    assert state["now"]==base+datetime.timedelta(microseconds=250001)
    assert state["waits"]==[.000001]
print("offsets equal; future waits [0.1, 0.1, 0.05]; reached/past waits []")
"#
        );
        let output = Command::new("python").args(["-c", &script]).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            "offsets equal; future waits [0.1, 0.1, 0.05]; reached/past waits []"
        );
        let cases = [
            ("2000-01-01T00:00:00Z", 0),
            ("bad", 1),
            ("2026-11-15T09:00:00", 1), // lint-rules: allow-literal naive timestamp rejection fixture
        ];
        for (stamp, code) in cases {
            let output = Command::new("python")
                .args(["-c", helper, "wait_until", stamp])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(code));
            if code == 0 {
                assert_eq!(
                    serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                    json!({"reached":true})
                );
            }
        }
    }
}
