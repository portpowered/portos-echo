use super::*;

#[test]
fn optional_profile_round_trips_both_loaders_and_model_precedence() {
    let directory = Directory::new().unwrap();
    for zone in ["Europe/Berlin", "UTC", "Etc/GMT+1", "Unknown/Syntax-valid_2"] {
        for effort in [
            "none",
            "minimal",
            "low",
            "medium",
            "high",
            "xhigh",
            "max",
            "ultra",
            "persistent",
            "custom",
            " ",
        ] {
            let input = format!(
                "{MINIMAL}\n[codex]\ndelegate_model='fixture-model'\ndelegate_effort='{effort}'\ntime_zone='{zone}'"
            );
            let path = directory.write(&input).unwrap();
            for config in [
                BridgeConfig::load(path).unwrap(),
                BridgeConfig::from_toml_str(&input).unwrap(),
            ] {
                assert_eq!(config.codex.delegate_model.as_deref(), Some("fixture-model"));
                assert_eq!(config.codex.delegate_effort.as_deref(), Some(effort));
                assert_eq!(config.codex.time_zone.as_deref(), Some(zone));
                assert_eq!(config.codex_model(), Some("fixture-model"));
            }
        }
    }
    let mut config =
        BridgeConfig::from_toml_str(&format!("{MINIMAL}\n[codex]\ndelegate_model='delegate'\nmodel='codex'")).unwrap();
    config.agent.model = Some("agent".into());
    for expected in [Some("delegate"), Some("codex"), Some("agent"), None] {
        assert_eq!(config.codex_model(), expected);
        if config.codex.delegate_model.take().is_none() && config.codex.model.take().is_none() {
            config.agent.model = None;
        }
    }
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn invalid_profile_values_are_sanitized_offline() {
    let directory = Directory::new().unwrap();
    for (field, values, message) in [
        ("delegate_model", vec!["''", "'   '"], "must be nonblank"),
        ("delegate_effort", vec!["''"], "must be nonempty"),
        (
            "time_zone",
            vec![
                "''",
                "'/UTC'",
                "'UTC/'",
                "'A//B'",
                "'A/../B'",
                "'A.B'",
                "'A B'",
                "'A\\B'",
                "'\u{8a9e}'",
                "'A[B]'",
                "'A\"B'",
                "\"A\\nB\"",
            ],
            "expected IANA-name syntax",
        ),
    ] {
        for (value, message) in values
            .into_iter()
            .map(|v| (v, message))
            .chain(["false", "12", "[]", "{}"].map(|v| (v, "invalid type or value")))
        {
            let input = format!("{MINIMAL}\n[codex]\n{field}={value}");
            let path = directory.write(&input).unwrap();
            for result in [BridgeConfig::load(path), BridgeConfig::from_toml_str(&input)] {
                let error = result.unwrap_err();
                assert_eq!(error.to_string(), format!("codex.{field}: {message}"));
                assert!(error.source().is_none());
            }
        }
    }
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    assert!(!directory.0.join("absent").exists());
}

#[test]
fn documented_profile_can_be_enabled_without_changing_defaults() {
    let input = EXAMPLE
        .replace("# [codex]\n", "[codex]\n")
        .replace("# delegate_model =", "delegate_model =")
        .replace("# delegate_effort =", "delegate_effort =")
        .replace("# delegate_fast =", "delegate_fast =")
        .replace("# time_zone =", "time_zone =");
    let directory = Directory::new().unwrap();
    let path = directory.write(&input).unwrap();
    for config in [
        BridgeConfig::load(path).unwrap(),
        BridgeConfig::from_toml_str(&input).unwrap(),
    ] {
        assert_eq!(config.codex_model(), Some("fixture-model"));
        assert_eq!(config.codex.delegate_effort.as_deref(), Some("low"));
        assert!(!config.codex.delegate_fast);
        assert_eq!(config.codex.time_zone.as_deref(), Some("America/Los_Angeles"));
    }
}

#[test]
fn fast_mode_defaults_off_and_loads_both_examples() {
    let directory = Directory::new().unwrap();
    let device_example = include_str!("../../../../examples/bridge.echo.toml");
    for input in [MINIMAL, EXAMPLE, device_example] {
        let parsed = BridgeConfig::from_toml_str(input).unwrap();
        assert!(!parsed.codex.delegate_fast);
        assert_eq!(BridgeConfig::load(directory.write(input).unwrap()).unwrap(), parsed);
    }
    for (value, expected) in [("true", true), ("false", false)] {
        let input = format!("{MINIMAL}\n[codex]\ndelegate_fast={value}");
        for config in [
            BridgeConfig::from_toml_str(&input).unwrap(),
            BridgeConfig::load(directory.write(&input).unwrap()).unwrap(),
        ] {
            assert_eq!(config.codex.delegate_fast, expected);
        }
    }
    let input = format!("{MINIMAL}\n[codex]\ndelegate_fast='synthetic'");
    for result in [
        BridgeConfig::from_toml_str(&input),
        BridgeConfig::load(directory.write(&input).unwrap()),
    ] {
        assert_eq!(
            result.unwrap_err().to_string(),
            "codex.delegate_fast: invalid type or value"
        );
    }
}

#[test]
fn trace_payload_defaults_and_overrides_load_without_other_file_access() {
    let directory = Directory::new().unwrap();
    for (section, expected) in [
        ("", true),
        ("[trace]", true),
        ("[trace]\npayloads=true", true),
        ("[trace]\npayloads=false", false),
    ] {
        let input = format!("{MINIMAL}\n{section}\n");
        let parsed = BridgeConfig::from_toml_str(&input).unwrap();
        assert_eq!(parsed.trace.payloads, expected);
        assert_eq!(BridgeConfig::load(directory.write(input).unwrap()).unwrap(), parsed);
    }
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    assert!(!directory.0.join("absent").exists());
}
