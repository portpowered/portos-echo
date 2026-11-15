#[cfg(test)]
mod tests {
    //! T3 through prebuilt CLI/tools; per-child fixtures and isolated directories.
    use serde_json::{Value, json};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const FAKE: &str = env!("CARGO_BIN_EXE_echo-lab-fake-run");

    struct Session(PathBuf);

    impl Session {
        fn new(fixture: &Value) -> Self {
            let root = std::env::temp_dir().join(format!(
                "lab-run-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::write(root.join("fixture.json"), fixture.to_string()).unwrap();
            let session = Self(root.canonicalize().unwrap());
            session.scenario("one.yaml", &json!([{"say":"utterance.wav"}]));
            fs::write(session.0.join("utterance.wav"), b"synthetic input fixture").unwrap();
            session
        }

        fn scenario(&self, name: &str, steps: &Value) {
            fs::write(
                self.0.join(name),
                json!({"id":"fixture", "milestone":"B0", "tiers":["T3"],
            "setup":{}, "steps":steps, "budgets":{}, "negative":[]})
                .to_string(),
            )
            .unwrap();
        }

        fn command(&self) -> Command {
            let mut command = Command::new(env!("CARGO_BIN_EXE_echo-lab"));
            command
                .current_dir(&self.0)
                .env("ECHO_LAB_RING", "fixture-ring")
                .env("ECHO_LAB_RUN_FIXTURE", self.0.join("fixture.json"))
                .env("ECHO_LAB_RUN_LEDGER", self.0.join("ledger.jsonl"));
            command
        }

        fn run_command(&self, extra: &[&str]) -> Command {
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

        fn run(&self, extra: &[&str]) -> Output {
            self.run_command(extra).output().unwrap()
        }

        fn ledger(&self) -> Vec<Value> {
            fs::read_to_string(self.0.join("ledger.jsonl"))
                .unwrap_or_default()
                .lines()
                .map(|s| serde_json::from_str(s).unwrap())
                .collect()
        }

        fn operations(&self) -> Vec<String> {
            self.ledger()
                .iter()
                .map(|v| v["operation"].as_str().unwrap().to_owned())
                .collect()
        }

        fn report(&self) -> Value {
            let report: Value =
                serde_json::from_slice(&fs::read(self.0.join("artifacts").join("report.json")).unwrap()).unwrap();
            let schema: Value = serde_json::from_str(include_str!("../../../schemas/report.schema.json")).unwrap();
            assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&report));
            report
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn failed(output: &Output, diagnostic: &str) {
        assert_eq!(output.status.code(), Some(1));
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(errors.contains(diagnostic), "{errors}");
        assert!(!errors.contains("fixture-private-child-output"));
    }

    #[test]
    fn f01_one_dispatch_hold_chirp_reply_release_and_atomic_report() {
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
                "digest", "firmware", "clock", "hold", "clock", "chirp", "reply", "clock", "play", "reply", "release"
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
            ledger[3]["args"],
            json!(["session", "hold", "fixture-ring", "--camera", "--listen", "--json"])
        );
        assert_eq!(
            ledger[5]["args"],
            json!([
                "message",
                "play-audio",
                "fixture-ring",
                "--lease",
                "fixture-lease",
                "--chirp",
                "--listen",
                "3",
                "--output",
                session.0.join("artifacts").join("alignment.wav"),
                "--json"
            ])
        );
        assert_eq!(
            ledger[8]["args"],
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
        assert_eq!(report["bench_session_ids"], json!([]));
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
        failed(&session.run(&[]), "before 09:00");
        assert_eq!(session.operations(), ["digest", "firmware", "clock"]);
        assert_eq!(session.report()["result"], "fail");
        let session = Session::new(&json!({"hour":9}));
        assert_eq!(session.run(&[]).status.code(), Some(0));
        assert_eq!(session.ledger().iter().filter(|v| v["operation"] == "play").count(), 1);
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
            (json!({"fail":"play"}), "play:", true),
            (json!({"malformed":"play"}), "play:", true),
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
    fn f06_clock_rechecked_before_chirp_and_every_utterance() {
        for (hours, expected) in [
            (
                json!([9, 8]),
                vec!["digest", "firmware", "clock", "hold", "clock", "release"],
            ),
            (
                json!([9, 9, 8]),
                vec![
                    "digest", "firmware", "clock", "hold", "clock", "chirp", "reply", "clock", "release",
                ],
            ),
        ] {
            let session = Session::new(&json!({"hours":hours}));
            failed(&session.run(&[]), "before 09:00");
            assert_eq!(session.operations(), expected);
        }
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
        for (op, count) in [("hold", 1), ("release", 1), ("chirp", 1), ("play", 2), ("firmware", 2)] {
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
        assert_eq!(session.operations(), ["digest", "firmware"]);
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
}
