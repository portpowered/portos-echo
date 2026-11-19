//! Voice overlay witnesses use authored M2 steps; session runtime stays unclaimed.
use super::*;

fn with_budgets(session: &Session, budgets: Value) -> PathBuf {
    let mut authored: Value = serde_norway::from_str(&fs::read_to_string(scenario()).unwrap()).unwrap();
    authored["budgets"] = budgets;
    let source = scenario().parent().unwrap().join("../fixtures/lab/w1-utterance.pcm");
    fs::copy(source, session.0.join("utterance.pcm")).unwrap();
    authored["steps"][0]["say"] = "utterance.pcm".into();
    let path = session.0.join("budgets.yaml");
    fs::write(&path, authored.to_string()).unwrap();
    path
}

fn budget_command(session: &Session, real: bool, path: &Path) -> Command {
    let original = command(session, real);
    let args: Vec<_> = original.get_args().map(|arg| arg.to_owned()).collect();
    let mut command = session.command();
    command.arg("run").arg(path).args(&args[2..]);
    command
}

#[test]
fn t3_device_t5_voice_budget_copy_preserves_real_paths_and_operator_file() {
    for real in [false, true] {
        let session = prepare(&json!({}));
        let path = session.0.join("bridge.toml");
        let mut contents = fs::read_to_string(&path).unwrap();
        if real {
            contents.push_str("\n[codex]\nhome = 'protected/home'\n");
        }
        fs::write(&path, &contents).unwrap();
        let scenario = with_budgets(&session, json!({"session_listen_seconds":3,"session_max_seconds":30}));
        let output = budget_command(&session, real, &scenario).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let launch: Value = serde_json::from_slice(&fs::read(session.0.join("launch.json")).unwrap()).unwrap();
        let config = echo_config::BridgeConfig::from_toml_str(launch["config"].as_str().unwrap()).unwrap();
        assert_eq!(config.voice.session_listen_seconds, 3);
        assert_eq!(config.voice.session_max_seconds, 30);
        if real {
            assert_real_launch(&session, &launch, &contents, &config);
        }
        assert_eq!(fs::read_to_string(path).unwrap(), contents);
        assert!(!session.0.join("protected").exists());
        assert!(!session.0.join("artifacts/configured.private").exists());
        assert_eq!(session.report()["result"], "pass");
        let rows = observations(&session);
        let effective: Vec<_> = rows
            .iter()
            .filter(|row| row["event"] == "device.effective_voice")
            .collect();
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0]["session_listen_seconds"], 3);
        assert_eq!(effective[0]["session_max_seconds"], 30);
    }
}

fn assert_real_launch(session: &Session, launch: &Value, contents: &str, config: &echo_config::BridgeConfig) {
    let mut expected = echo_config::BridgeConfig::from_toml_str(contents).unwrap();
    expected.voice = config.voice;
    assert_eq!(config, &expected);
    assert_eq!(launch["cwd"], session.0.to_string_lossy().as_ref());
    assert_eq!(
        launch["args"][1],
        session
            .0
            .join("artifacts/configured.private/bridge.toml")
            .to_string_lossy()
            .as_ref()
    );
    assert!(
        !launch["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--live-loopback" || arg == "--model-loopback")
    );
}

#[test]
fn t3_device_t5_invalid_voice_budgets_refuse_before_effects() {
    for budgets in [
        json!({"session_listen_seconds":2}),
        json!({"session_listen_seconds":3.5}),
        json!({"session_max_seconds":2}),
        json!({"session_listen_seconds":30,"session_max_seconds":3}),
        json!({"unknown_voice_setting":3}),
    ] {
        let session = prepare(&json!({}));
        let path = with_budgets(&session, budgets);
        let output = budget_command(&session, true, &path).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            session
                .operations()
                .iter()
                .filter(|op| *op == "bridge" || *op == "play" || *op == "hold")
                .count(),
            0
        );
        assert!(!session.0.join("artifacts").exists());
    }
}
