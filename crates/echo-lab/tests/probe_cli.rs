#[cfg(test)]
mod probe_support;
// Component T3 calibration cases share this prebuilt-fake test binary and its
// existing nextest process-heavy concurrency group. No production dispatch yet.
mod lab_probe {
    pub(crate) mod probe_command {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lab_probe/probe_command.rs"));
    }
    pub(crate) mod probe_stream {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lab_probe/probe_stream.rs"));
    }
    pub(crate) mod probe_parse {
        pub(crate) mod probe_inputs {
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lab_probe/probe_inputs.rs"));
        }
    }
}
mod lab_calibrate {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lab_calibrate/mod.rs"));
}
#[cfg(test)]
mod tests {
    use super::probe_support;
    use probe_support::{FAKE, Session, manifest, response};
    use serde_json::{Value, json};
    use std::fs;
    use std::process::Command;

    #[test]
    fn probe_scrubs_serial_everywhere() {
        let session = Session::new(&json!({}));
        let output = session.run();
        let value = manifest(&session, &output);
        assert_eq!(
            value["build"],
            json!({"device":"athena", "sdk":28,
        "incremental":"fixture-incremental", "kernel":"fixture-kernel",
        "fingerprint":"fixture/athena/<serial>:userdebug/test"})
        );
        assert_eq!(value["source"], concat!("echo-lab probe ", env!("CARGO_PKG_VERSION")));
        assert!(value["probe_timestamp"].as_str().unwrap().ends_with('Z'));
        assert_eq!(value["display"], json!({"width":1280,"height":800,"density":213}));
        assert_eq!(
            value["mixer"],
            json!({"name":"Master TAS5805m Playback Volume", "values":[100]})
        );
        assert_eq!(value["keyword_models"], json!(["en-US", "test-locale"]));
        assert_eq!(
            value["input_devices"],
            json!([
                {"name":"amazon_touch", "capabilities":["ABS=260800000000000", "EV=b", "EV_ABS", "X=0..1279", "Y=0..799"]},
                {"name":"fts_ts", "capabilities":["ABS=260800000000000", "EV=b", "EV_ABS", "X=0..1280", "Y=0..800"]}
            ])
        );
        check_hashes(&value);
        probe_support::clean(&output.stdout);
        probe_support::clean(&output.stderr);
        assert_eq!(probe_support::scan(&session.0.join("out")), 42);
        assert_eq!(fs::read(session.0.join("out/raw/.gitignore")).unwrap(), b"*\n");
        assert_eq!(
            fs::read_to_string(session.0.join("out/raw/00.stdout.txt")).unwrap(),
            "<serial>\n"
        );
        assert_eq!(
            fs::read_to_string(session.0.join("out/raw/16.stdout.txt")).unwrap(),
            "Synthetic input corroboration <serial> / <serial><serial>\n"
        );
        assert_eq!(
            fs::read_to_string(session.0.join("out/raw/05.stderr.txt")).unwrap(),
            "fixture <serial> <serial> <serial>\n"
        );
        check_reads(&session);
    }

    fn check_hashes(value: &Value) {
        let expected = [
            (
                "/system/lib/libpryon.so",
                "11111111111111111111111111111111",
                "3d687c25dacce49f68453b898be22cc5",
            ),
            (
                "/system/vendor/lib/libasp.so",
                "ac5e8985aa9d67461d3b054bde179363",
                "ac5e8985aa9d67461d3b054bde179363",
            ),
            (
                "/system/lib/libaspclient.so",
                "aeb4f3dc3d9ace3157d5c36bf9739836",
                "aeb4f3dc3d9ace3157d5c36bf9739836",
            ),
            (
                "/system/lib/libaudioflinger.so",
                "22222222222222222222222222222222",
                "78785db566775d7740e23adbcd71538d",
            ),
            (
                "/system/vendor/lib/hw/audio.primary.mt8183.so",
                "cc54a24e1499090ac648adccae2033dd",
                "cc54a24e1499090ac648adccae2033dd",
            ),
            (
                "/system/lib/libwakewordserver_jni.so",
                "0926fadcaefc0b8723a6d7d5988cae54",
                "0926fadcaefc0b8723a6d7d5988cae54",
            ),
        ];
        assert_eq!(value["hashes"].as_array().unwrap().len(), 6);
        for (hash, (path, md5, pinned)) in value["hashes"].as_array().unwrap().iter().zip(expected) {
            assert_eq!(
                *hash,
                json!({"path":path, "md5":md5, "pinned_md5":pinned,
            "comparison":if md5 == pinned {"match"} else {"differs"}})
            );
        }
    }

    fn check_reads(session: &Session) {
        let reads: Vec<Vec<String>> = serde_json::from_str(r#"[
        ["shell","getprop","ro.serialno"], ["shell","getprop","ro.boot.serialno"],
        ["shell","getprop","ro.product.device"], ["shell","getprop","ro.build.version.incremental"],
        ["shell","getprop","ro.build.version.sdk"], ["shell","getprop","ro.build.fingerprint"],
        ["shell","uname","-r"], ["shell","md5sum","/system/lib/libpryon.so"],
        ["shell","md5sum","/system/vendor/lib/libasp.so"], ["shell","md5sum","/system/lib/libaspclient.so"],
        ["shell","md5sum","/system/lib/libaudioflinger.so"], ["shell","md5sum","/system/vendor/lib/hw/audio.primary.mt8183.so"],
        ["shell","md5sum","/system/lib/libwakewordserver_jni.so"], ["shell","getevent","-lp"],
        ["shell","cat","/proc/bus/input/devices"], ["shell","tinymix"], ["shell","dumpsys","input"],
        ["shell","wm","size"], ["shell","wm","density"], ["shell","ls","/system/local/models/keyword"]
    ]"#).unwrap();
        assert_eq!(session.ledger(), reads.iter().map(|r| json!(r)).collect::<Vec<_>>());
    }

    #[test]
    fn complete_probe_ledger_is_unchanged_by_calibration_reuse() {
        let session = Session::new(&json!({}));
        let output = session.run();
        assert_eq!(
            manifest(&session, &output)["display"],
            json!({"width":1280,"height":800,"density":213})
        );
        check_reads(&session);
    }

    #[test]
    fn empty_lists_and_empty_serial_properties_are_explicit() {
        for both_empty in [false, true] {
            let session = Session::new(&json!({
                "shell getprop ro.serialno":response(""),
                "shell getprop ro.boot.serialno":response(if both_empty {""} else {"FAKESERIAL456\n"}),
                "shell getprop ro.build.fingerprint":response("fixture/no-identifier"),
                "shell getevent -lp":response(""), "shell cat /proc/bus/input/devices":response(""),
                "shell ls /system/local/models/keyword":response("")
            }));
            let output = session.run();
            let value = manifest(&session, &output);
            assert_eq!(value["input_devices"], json!([]));
            assert_eq!(value["keyword_models"], json!([]));
            assert_eq!(value["build"]["fingerprint"], "fixture/no-identifier");
            // An unreported identifier cannot be inferred from arbitrary child text.
            // F-7 only claims known-serial replacement; empty needles are ignored.
            if !both_empty {
                let text = fs::read_to_string(session.0.join("out/raw/16.stdout.txt")).unwrap();
                assert!(!text.contains("FAKESERIAL456"));
            }
        }
    }

    #[test]
    fn prior_output_is_immutable_and_contending_callers_have_one_winner() {
        let session = Session::new(&json!({}));
        fs::create_dir(session.0.join("out")).unwrap();
        fs::write(session.0.join("out/sentinel"), "keep").unwrap();
        assert_eq!(session.run().status.code(), Some(1));
        assert_eq!(fs::read(session.0.join("out/sentinel")).unwrap(), b"keep");
        assert_eq!(session.ledger(), Vec::<Value>::new());
        let destination = session.0.join("race");
        let first = session
            .command(&destination)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let second = session.command(&destination).output().unwrap();
        let first = first.wait_with_output().unwrap();
        let mut codes = [first.status.code(), second.status.code()];
        codes.sort_unstable();
        assert_eq!(codes, [Some(0), Some(1)]);
        let winner = if first.status.success() { first } else { second };
        assert_eq!(fs::read(destination.join("manifest.json")).unwrap(), winner.stdout);
        assert_eq!(probe_support::scan(&destination), 42);
        independent_probes_keep_their_own_observations();
    }

    fn independent_probes_keep_their_own_observations() {
        let sessions = ["incremental-a", "incremental-b"]
            .map(|marker| Session::new(&json!({"shell getprop ro.build.version.incremental":response(marker)})));
        let children = sessions.each_ref().map(|session| {
            session
                .command(&session.0.join("out"))
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        });
        let outputs = children.map(|child| child.wait_with_output().unwrap());
        for ((session, output), marker) in sessions.iter().zip(&outputs).zip(["incremental-a", "incremental-b"]) {
            let value = manifest(session, output);
            assert_eq!(value["build"]["incremental"], marker);
            check_reads(session);
            probe_support::clean(&output.stdout);
            probe_support::clean(&output.stderr);
            assert_eq!(probe_support::scan(&session.0.join("out")), 42);
        }
    }

    #[test]
    fn probe_grammar_prevents_reads_and_allows_reversed_flags() {
        let session = Session::new(&json!({}));
        for args in [
            vec!["probe"],
            vec!["probe", "--adb", FAKE],
            vec!["probe", "--adb", FAKE, "--adb", FAKE],
            vec!["probe", "--out", "out", "--other", FAKE],
            vec!["probe", "--adb", FAKE, "--out", "out", "extra"],
        ] {
            let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
                .args(args)
                .current_dir(&session.0)
                .env("ECHO_LAB_FAKE_LEDGER", session.0.join("ledger.jsonl"))
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(output.stdout, b"");
            assert!(String::from_utf8(output.stderr).unwrap().contains("probe requires"));
        }
        assert_eq!(session.ledger(), Vec::<Value>::new());
        let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
            .args(["probe", "--out", "out", "--adb", FAKE])
            .current_dir(&session.0)
            .output()
            .unwrap();
        assert_eq!(manifest(&session, &output)["display"]["width"], 1280);
        let help = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
            .arg("--help")
            .output()
            .unwrap();
        assert!(
            String::from_utf8(help.stdout)
                .unwrap()
                .contains("echo-lab probe --adb <path-to-adb> --out <dir>")
        );
    }

    fn failed_probe(overrides: &Value, code: i32, label: &str, reads: usize) {
        let session = Session::new(overrides);
        let output = session.run();
        assert_eq!(output.status.code(), Some(code));
        assert_eq!(output.stdout, b"");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(label), "{error}");
        probe_support::clean(&output.stderr);
        assert!(!session.0.join("out/manifest.json").exists());
        assert_eq!(session.ledger().len(), reads);
    }

    #[test]
    fn refuses_wrong_device_and_scrubs_read_failure_diagnostics() {
        failed_probe(
            &json!({"shell getprop ro.product.device":response("other-FAKESERIAL123")}),
            2,
            "expected athena",
            3,
        );
        failed_probe(
            &json!({"shell uname -r":{"stdout":"FAKESERIAL123", "stderr":"denied FAKESERIAL123FAKESERIAL456", "code":1}}),
            1,
            "probe uname -r: read failed: denied <serial><serial>",
            7,
        );
    }

    #[test]
    fn either_serial_discovery_failure_discards_all_child_output() {
        for (index, command) in ["shell getprop ro.serialno", "shell getprop ro.boot.serialno"]
            .iter()
            .enumerate()
        {
            failed_probe(
                &json!({*command:{"stdout":"FAKESERIAL123", "stderr":"FAKESERIAL456 private diagnostic", "code":1}}),
                1,
                "probe serial discovery failed; child output discarded",
                index + 1,
            );
        }
        failed_probe(
            &json!({"shell getprop ro.boot.serialno":response("two tokens\n")}),
            1,
            "child output discarded",
            2,
        );
    }

    #[test]
    fn malformed_required_observations_never_publish_a_manifest() {
        for (command, text, error) in [
            ("shell md5sum /system/lib/libpryon.so", "malformed", "probe md5sum"),
            ("shell tinymix", "83 INT 1 Wrong Control 100", "missing named control"),
            ("shell wm size", "malformed", "probe display size"),
            ("shell wm density", "Physical density: 0", "display density"),
            ("shell getprop ro.build.version.sdk", "zero", "probe sdk"),
            ("shell getprop ro.build.version.incremental", "", "probe incremental"),
            ("shell getprop ro.build.fingerprint", "", "probe fingerprint"),
            ("shell uname -r", "", "probe kernel"),
            (
                "shell getevent -lp",
                "add device 1:\nname: missing quotes",
                "input devices",
            ),
            ("shell getevent -lp", "no named device", "probe getevent"),
            ("shell cat /proc/bus/input/devices", "unrecognized", "probe proc input"),
        ] {
            failed_probe(&json!({command:response(text)}), 1, error, 20);
        }
    }

    #[test]
    fn launch_and_local_output_errors_preserve_existing_files() {
        let session = Session::new(&json!({}));
        let output = Command::new(env!("CARGO_BIN_EXE_echo-lab"))
            .args(["probe", "--adb", "missing-fake-executable", "--out", "out"])
            .current_dir(&session.0)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            output.stderr,
            b"echo-lab: probe serial discovery failed; child output discarded\n"
        );
        assert!(!session.0.join("out").exists());
        fs::write(session.0.join("parent-file"), "sentinel").unwrap();
        for path in [session.0.join("parent-file/out"), session.0.join("absent-parent/out")] {
            let output = session.command(&path).output().unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(output.stdout, b"");
            assert!(String::from_utf8_lossy(&output.stderr).contains("probe output:"));
            assert!(!path.join("manifest.json").exists());
        }
        assert_eq!(fs::read(session.0.join("parent-file")).unwrap(), b"sentinel");
        assert!(!session.0.join("manifest.json").exists());
    }

    #[test]
    fn broken_stdout_returns_failure_and_retains_completed_file() {
        let session = Session::new(&json!({}));
        let mut child = session
            .command(&session.0.join("out"))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdout.take());
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stderr, b"echo-lab: probe stdout: write failed\n");
        let value: Value = serde_json::from_slice(&fs::read(session.0.join("out/manifest.json")).unwrap()).unwrap();
        assert_eq!(value["build"]["device"], "athena");
        assert_eq!(probe_support::scan(&session.0.join("out")), 42);
    }
}
