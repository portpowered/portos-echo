use super::*;
use echo_config::SettingsOverlay;

fn overlay(input: &str) -> SettingsOverlay {
    let Ok(overlay) = SettingsOverlay::from_toml_str(input) else {
        unreachable!("invalid settings fixture")
    };
    overlay
}

#[test]
fn screen_settings_muse_overlay_masks_base_and_refuses_unsafe_references() {
    let base = BridgeConfig::from_toml_str(&format!(
        "{MINIMAL}\n[muse]\nurl='https://127.0.0.1:1'\nca_file='owner.der'\ncredential_file='owner-key'\n"
    ))
    .unwrap();
    assert_eq!(SettingsOverlay::default().apply_to(&base).unwrap().muse, base.muse);
    let clear = overlay("[muse]\nenabled=false");
    assert_eq!(clear.apply_to(&base).unwrap().muse, None);
    assert_eq!(clear.to_toml_string().unwrap(), "[muse]\nenabled = false\n");
    for ca in [
        "/muse/connection-1/ca.der",
        "muse/../connection-1/ca.der",
        "muse//connection-1/ca.der",
        "muse/connection-2/ca.der",
        "muse/connection-1/../ca.der",
    ] {
        let text = format!(
            "[muse]\nurl='https://127.0.0.1:1'\nca_file='{ca}'\ncredential_file='muse/connection-1/credential'\n"
        );
        assert_eq!(
            SettingsOverlay::from_toml_str(&text).unwrap_err().to_string(),
            "muse: expected private connection files under home"
        );
    }
    for input in [
        "[muse]\nenabled=true",
        "[muse]\nenabled=false\ncredential_file='outside'",
    ] {
        assert!(SettingsOverlay::from_toml_str(input).is_err());
    }
}

#[cfg(unix)]
#[test]
fn screen_settings_muse_overlay_rejects_symlinked_generation_and_file() {
    use std::{fs, os::unix::fs::symlink};
    let root = Directory::new().unwrap();
    let mut base = BridgeConfig::from_toml_str(MINIMAL).unwrap();
    let home = root.0.join("home");
    let owned = home.join("muse/connection-1");
    let outside = root.0.join("outside");
    fs::create_dir_all(owned.parent().unwrap()).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("ca.der"), b"fixture").unwrap();
    fs::write(outside.join("credential"), b"fixture").unwrap();
    base.home = Some(echo_config::HomeConfig {
        dir: home,
        agent_tools: echo_config::AgentTools::default(),
    });
    let saved = overlay(
        "[muse]\nurl='https://127.0.0.1:1'\nca_file='muse/connection-1/ca.der'\ncredential_file='muse/connection-1/credential'\n",
    );
    symlink(&outside, &owned).unwrap();
    assert!(saved.apply_to(&base).is_err());
    fs::remove_file(&owned).unwrap();
    fs::create_dir(&owned).unwrap();
    fs::write(owned.join("ca.der"), b"fixture").unwrap();
    symlink(outside.join("credential"), owned.join("credential")).unwrap();
    assert!(saved.apply_to(&base).is_err());
    fs::remove_file(owned.join("credential")).unwrap();
    fs::write(owned.join("credential"), b"fixture").unwrap();
    assert_eq!(
        saved.apply_to(&base).unwrap().muse.unwrap().ca_file,
        owned.join("ca.der")
    );
}

#[test]
fn screen_settings_followup_defaults_on_and_only_accepts_booleans() {
    assert!(
        BridgeConfig::from_toml_str(MINIMAL)
            .unwrap()
            .voice
            .session_listen_enabled
    );
    for enabled in [true, false] {
        let input = format!("{MINIMAL}\n[voice]\nsession_listen_enabled={enabled}");
        assert_eq!(
            BridgeConfig::from_toml_str(&input)
                .unwrap()
                .voice
                .session_listen_enabled,
            enabled
        );
    }
    for value in ["0", "1", "'false'", "[]", "{}"] {
        let input = format!("{MINIMAL}\n[voice]\nsession_listen_enabled={value}");
        assert_eq!(
            BridgeConfig::from_toml_str(&input).unwrap_err().to_string(),
            "voice.session_listen_enabled: invalid type or value"
        );
    }
    let base = BridgeConfig::from_toml_str(MINIMAL).unwrap();
    for input in ["", "# no overrides", "[voice]\n[agent]\n[codex]"] {
        let empty = overlay(input);
        assert_eq!(empty, SettingsOverlay::default());
        assert_eq!(empty.apply_to(&base).unwrap(), base);
        assert_eq!(empty.to_toml_string().unwrap(), "");
    }
}

#[test]
fn screen_settings_sparse_merge_preserves_base_and_only_explicit_keys() {
    let base = BridgeConfig::from_toml_str(MINIMAL).unwrap();
    let first = overlay("[agent]\nmax_tool_calls=4\n");
    assert_eq!(first.to_toml_string().unwrap(), "[agent]\nmax_tool_calls = 4\n");
    let second = first.merge(&overlay("[voice]\nsession_listen_enabled=false\n"));
    let third = second.merge(&overlay("[agent]\nmax_tool_calls=1\n"));
    let applied = third.apply_to(&base).unwrap();
    assert_eq!(third.legacy_tool_cap(), Some(1));
    assert_eq!(applied.agent, base.agent);
    assert!(!applied.voice.session_listen_enabled);
    assert_eq!(applied.voice.session_listen_seconds, 15);
    assert_eq!(applied.voice.session_max_seconds, 600);
    assert_eq!(applied.agent.daily_request_cap, 500);
    assert_eq!(applied.agent.task_deadline_seconds, 60);
    assert_eq!(
        (&applied.codex, &applied.live, &applied.mcp, &applied.screen),
        (&base.codex, &base.live, &base.mcp, &base.screen)
    );
    assert_eq!(base.agent.max_tool_calls, 8);
    assert!(base.voice.session_listen_enabled);
    assert_eq!(overlay(&third.to_toml_string().unwrap()), third);
    assert_eq!(
        third.to_toml_string().unwrap(),
        "[agent]\nmax_tool_calls = 1\n\n[voice]\nsession_listen_enabled = false\n"
    );
}

#[test]
fn screen_settings_voices_preserve_precedence_and_the_pinned_default() {
    let base = BridgeConfig::from_toml_str(&MINIMAL.replace("[live]", "[live]\nvoice='cedar'")).unwrap();
    for voice in ["default", "alloy", "marin", "cove", "verse"] {
        let settings = overlay(&format!("[codex]\nvoice='{voice}'"));
        let config = settings.apply_to(&base).unwrap();
        assert_eq!(config.live.voice.as_deref(), Some("cedar"));
        assert_eq!(config.codex_voice(), (voice != "default").then_some(voice));
    }
    for (voice, expected) in [
        ("'SYNTHETIC_VALUE'", "expected default or a pinned realtime voice"),
        ("'alloy '", "expected default or a pinned realtime voice"),
        ("''", "must be nonblank"),
        ("'  '", "must be nonblank"),
    ] {
        let error = overlay(&format!("[codex]\nvoice={voice}")).apply_to(&base).unwrap_err();
        assert_eq!(error.to_string(), format!("codex.voice: {expected}"));
        assert!(!format!("{error:?}").contains("SYNTHETIC_VALUE"));
    }
}

#[test]
fn screen_settings_empty_model_and_effort_select_pin_defaults_over_legacy_aliases() {
    let mut base = BridgeConfig::from_toml_str(MINIMAL).unwrap();
    base.agent.model = Some("agent-alias".into());
    base.codex.model = Some("codex-alias".into());
    base.codex.delegate_effort = Some("low".into());

    let untouched = overlay("[agent]\nmax_tool_calls=4").apply_to(&base).unwrap();
    assert_eq!(untouched.codex_model(), Some("codex-alias"));
    assert_eq!(untouched.codex.delegate_effort.as_deref(), Some("low"));

    let cleared = overlay("[codex]\ndelegate_model=''\ndelegate_effort=''");
    let effective = cleared.apply_to(&base).unwrap();
    assert_eq!(effective.codex_model(), None);
    assert_eq!(effective.codex.delegate_effort, None);
    assert_eq!(overlay(&cleared.to_toml_string().unwrap()), cleared);
    assert_eq!(base.codex_model(), Some("codex-alias"));
    assert_eq!(base.agent.model.as_deref(), Some("agent-alias"));
}

#[test]
fn screen_settings_fast_mode_preserves_inheritance_and_explicit_false() {
    let base = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n[codex]\ndelegate_fast=true")).unwrap();
    let other = overlay("[codex]\ndelegate_effort='low'");
    assert!(other.apply_to(&base).unwrap().codex.delegate_fast);
    let settings = other.merge(&overlay("[codex]\ndelegate_fast=false"));
    let encoded = settings.to_toml_string().unwrap();
    assert_eq!(encoded, "[codex]\ndelegate_effort = \"low\"\ndelegate_fast = false\n");
    let effective = overlay(&encoded).apply_to(&base).unwrap();
    assert!(!effective.codex.delegate_fast);
    assert_eq!(effective.codex.delegate_effort.as_deref(), Some("low"));
    assert!(base.codex.delegate_fast);
    let bad = "[agent]\nmax_tool_calls=3\n[codex]\ndelegate_fast='false'";
    assert_eq!(
        SettingsOverlay::from_toml_str(bad).unwrap_err().to_string(),
        "codex.delegate_fast: invalid type or value"
    );
}

#[test]
fn screen_settings_sparse_merge_cannot_encode_an_overlay_that_startup_would_refuse() {
    let model = overlay(&format!("[codex]\ndelegate_model='{}'", "m".repeat(33_000)));
    let effort = overlay(&format!("[codex]\ndelegate_effort='{}'", "e".repeat(33_000)));
    assert_eq!(overlay(&model.to_toml_string().unwrap()), model);
    assert_eq!(overlay(&effort.to_toml_string().unwrap()), effort);
    let error = model.merge(&effort).to_toml_string().unwrap_err();
    assert_eq!(error.to_string(), "settings.toml: overlay exceeds 65536 bytes");
}

#[test]
fn screen_settings_range_boundaries_and_cross_field_validation_use_real_config() {
    let base = BridgeConfig::from_toml_str(MINIMAL).unwrap();
    for (setting, expected) in [
        (
            "[voice]\nsession_listen_seconds=2",
            "voice.session_listen_seconds: must be within 3..=300",
        ),
        (
            "[voice]\nsession_listen_seconds=301",
            "voice.session_listen_seconds: must be within 3..=300",
        ),
        (
            "[agent]\nmax_tool_calls=0",
            "agent.max_tool_calls: must be within 1..=32",
        ),
        (
            "[agent]\nmax_tool_calls=33",
            "agent.max_tool_calls: must be within 1..=32",
        ),
    ] {
        assert_eq!(overlay(setting).apply_to(&base).unwrap_err().to_string(), expected);
    }
    for seconds in [3, 300] {
        let config = overlay(&format!("[voice]\nsession_listen_seconds={seconds}"))
            .apply_to(&base)
            .unwrap();
        assert_eq!(config.voice.session_listen_seconds, seconds);
    }
    for cap in [0, 9_223_372_036_854_775_807_u64] {
        let settings = overlay(&format!("[agent]\ndaily_request_cap={cap}\nmax_tool_calls=32"));
        let config = settings.apply_to(&base).unwrap();
        assert_eq!(config.agent.daily_request_cap, cap);
        assert_eq!(settings.legacy_tool_cap(), Some(32));
        assert_eq!(overlay(&settings.to_toml_string().unwrap()), settings);
    }
    let short = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n[voice]\nsession_max_seconds=20")).unwrap();
    assert_eq!(
        overlay("[voice]\nsession_listen_seconds=21\nsession_listen_enabled=false")
            .apply_to(&short)
            .unwrap_err()
            .to_string(),
        "voice.session_max_seconds: must be at least voice.session_listen_seconds"
    );
}

const INVALID_SETTINGS: &[(&str, &str)] = &[
    ("version=1", "config: unknown field"),
    ("[SYNTHETIC_VALUE]\nkey='SYNTHETIC_VALUE'", "config: unknown field"),
    ("[voice]\nsession_max_seconds=600", "voice: unknown field"),
    ("[codex]\ninstructions='SYNTHETIC_VALUE'", "codex: unknown field"),
    ("[home]\ndir='SYNTHETIC_VALUE'", "home: unknown field"),
    ("[live]\nvoice='alloy'", "config: unknown field"),
    ("[screen]\nlisten='SYNTHETIC_VALUE'", "config: unknown field"),
    ("voice=[]", "voice: expected table"),
    (
        "[voice]\nsession_listen_enabled=0",
        "voice.session_listen_enabled: invalid type or value",
    ),
    (
        "[voice]\nsession_listen_seconds='SYNTHETIC_VALUE'",
        "voice.session_listen_seconds: invalid type or value",
    ),
    (
        "[codex]\nvoice={SYNTHETIC_VALUE='value'}",
        "codex.voice: invalid type or value",
    ),
    (
        "[agent]\nmax_tool_calls=-1",
        "agent.max_tool_calls: invalid type or value",
    ),
    (
        "[agent]\ndaily_request_cap=-1",
        "agent.daily_request_cap: invalid type or value",
    ),
    (
        "[agent]\ndaily_request_cap=9223372036854775808",
        "agent.daily_request_cap: must fit a TOML integer",
    ),
    (
        "[agent]\ndaily_request_cap=1.0",
        "agent.daily_request_cap: invalid type or value",
    ),
    (
        "[agent]\ndaily_request_cap='9007199254740993'",
        "agent.daily_request_cap: invalid type or value",
    ),
    (
        "[agent]\nmax_tool_calls=4\nmax_tool_calls=8",
        "settings.toml: invalid TOML syntax",
    ),
    ("[SYNTHETIC_VALUE", "settings.toml: invalid TOML syntax"),
];

#[test]
fn screen_settings_unknown_keys_tables_and_types_have_payload_free_errors() {
    for &(input, expected) in INVALID_SETTINGS {
        let error = SettingsOverlay::from_toml_str(input).unwrap_err();
        assert_eq!(error.to_string(), expected, "{input}");
        assert!(!format!("{error:?}").contains("SYNTHETIC_VALUE"));
    }
}

fn home_config(directory: &Directory) -> (PathBuf, String) {
    let input = format!("{MINIMAL}\n[home]\ndir='{}'", directory.0.display());
    let Ok(path) = directory.write(&input) else {
        unreachable!("could not write settings fixture")
    };
    (path, input)
}

#[test]
fn screen_settings_effective_load_overrides_only_the_allowlist_without_writes() {
    let directory = Directory::new().unwrap();
    let (path, input) = home_config(&directory);
    let overlay_path = directory.0.join("settings.toml");
    let text = "[agent]\nmax_tool_calls=2\ntask_deadline_seconds=5\ndaily_request_cap=9007199254740993\n[voice]\nsession_listen_enabled=false\nsession_listen_seconds=5\n[codex]\nvoice='marin'";
    std::fs::write(&overlay_path, text).unwrap();
    let base = BridgeConfig::from_toml_str(&input).unwrap();
    assert_eq!(BridgeConfig::load(&path).unwrap(), base);
    assert_eq!(base.agent.max_tool_calls, 8);
    for _ in 0..2 {
        let effective = BridgeConfig::load_effective(&path).unwrap();
        assert!(effective.warning.is_none());
        assert_eq!(effective.base, base);
        assert_eq!(effective.overlay, overlay(text));
        assert_eq!(effective.overlay.legacy_tool_cap(), Some(2));
        assert_eq!(
            effective.config.agent.task_deadline_seconds,
            base.agent.task_deadline_seconds
        );
        assert_eq!(effective.config.agent.max_tool_calls, base.agent.max_tool_calls);
        assert_eq!(effective.config, overlay(text).apply_to(&base).unwrap());
        assert_eq!(effective.config.agent.daily_request_cap, 9_007_199_254_740_993);
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), input);
    assert_eq!(std::fs::read_to_string(overlay_path).unwrap(), text);
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 2);
}

#[test]
fn screen_settings_invalid_overlay_is_ignored_as_a_whole_with_safe_warning() {
    let directory = Directory::new().unwrap();
    let (path, input) = home_config(&directory);
    let base = BridgeConfig::from_toml_str(&input).unwrap();
    for (bytes, expected) in [
        (
            b"SYNTHETIC_VALUE='unterminated".to_vec(),
            "settings.toml: invalid TOML syntax",
        ),
        (
            b"[agent]\nmax_tool_calls=4\n[voice]\nsession_listen_seconds=0".to_vec(),
            "voice.session_listen_seconds: must be within 3..=300",
        ),
        (
            b"[agent]\nmax_tool_calls=4\n[home]\ndir='SYNTHETIC_VALUE'".to_vec(),
            "home: unknown field",
        ),
        (
            b"[agent]\nmax_tool_calls=4\n[codex]\ndelegate_fast='SYNTHETIC_VALUE'".to_vec(),
            "codex.delegate_fast: invalid type or value",
        ),
        (vec![255], "settings.toml: expected UTF-8 overlay"),
        (vec![b' '; 65_537], "settings.toml: overlay exceeds 65536 bytes"),
    ] {
        let overlay_path = directory.0.join("settings.toml");
        std::fs::write(&overlay_path, &bytes).unwrap();
        let effective = BridgeConfig::load_effective(&path).unwrap();
        assert_eq!(effective.config, base);
        assert_eq!(effective.base, base);
        assert_eq!(effective.overlay, SettingsOverlay::default());
        let error = effective.warning.unwrap();
        assert_eq!(error.to_string(), format!("settings.toml: ignored overlay: {expected}"));
        assert!(!format!("{error:?}").contains("SYNTHETIC_VALUE"));
        assert_eq!(std::fs::read(overlay_path).unwrap(), bytes);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), input);
    }
}

#[test]
fn screen_settings_absent_home_missing_overlay_and_bad_base_preserve_load_contract() {
    let directory = Directory::new().unwrap();
    let path = directory.write(MINIMAL).unwrap();
    std::fs::write(directory.0.join("settings.toml"), "invalid [").unwrap();
    let effective = BridgeConfig::load_effective(&path).unwrap();
    assert_eq!(effective.config, BridgeConfig::from_toml_str(MINIMAL).unwrap());
    assert!(effective.warning.is_none());
    let input = format!("{MINIMAL}\n[home]\ndir='{}'", directory.0.join("missing").display());
    let path = directory.write(&input).unwrap();
    let effective = BridgeConfig::load_effective(&path).unwrap();
    assert_eq!(effective.config, BridgeConfig::from_toml_str(&input).unwrap());
    assert!(effective.warning.is_none());
    assert!(!directory.0.join("missing").exists());
    directory.write("[invalid").unwrap();
    assert_eq!(
        BridgeConfig::load_effective(&path).unwrap_err().to_string(),
        BridgeConfig::load(&path).unwrap_err().to_string()
    );
}

#[test]
fn screen_settings_nonregular_overlay_is_ignored_without_opening_it() {
    let directory = Directory::new().unwrap();
    let (path, _) = home_config(&directory);
    std::fs::create_dir(directory.0.join("settings.toml")).unwrap();
    let effective = BridgeConfig::load_effective(path).unwrap();
    assert_eq!(effective.config.agent.max_tool_calls, 8);
    assert_eq!(
        effective.warning.unwrap().to_string(),
        "settings.toml: ignored overlay: settings.toml: expected regular file"
    );
}

#[cfg(unix)]
#[test]
fn screen_settings_symlink_overlay_cannot_read_a_different_file() {
    let directory = Directory::new().unwrap();
    let (path, input) = home_config(&directory);
    let target = directory.0.join("other.toml");
    let text = "[agent]\nmax_tool_calls=4";
    std::fs::write(&target, text).unwrap();
    std::os::unix::fs::symlink(&target, directory.0.join("settings.toml")).unwrap();
    let effective = BridgeConfig::load_effective(&path).unwrap();
    assert_eq!(effective.config, BridgeConfig::from_toml_str(&input).unwrap());
    assert_eq!(
        effective.warning.unwrap().to_string(),
        "settings.toml: ignored overlay: settings.toml: expected regular file"
    );
    assert_eq!(std::fs::read_to_string(target).unwrap(), text);
}

#[test]
fn load_reports_legacy_agent_keys_without_applying_values() {
    let input = EXAMPLE;
    let directory = Directory::new().unwrap();
    let path = directory.0.join("bridge.toml");
    let legacy = input.replacen(
        "[agent]\n",
        "[agent]\nmax_tool_calls = 1\ntask_deadline_seconds = 5\n",
        1,
    );
    std::fs::write(&path, &legacy).unwrap();
    let (base, diagnostics) = BridgeConfig::load_with_warnings(&path).unwrap();
    let updated = BridgeConfig::from_toml_str(input).unwrap();
    assert_eq!(base, updated);
    assert_eq!(base.agent.daily_request_cap, 500);
    assert_eq!(base.voice.session_max_seconds, 600);
    assert_eq!(
        diagnostics.iter().map(ToString::to_string).collect::<Vec<_>>(),
        [
            "agent.max_tool_calls: deprecated and ignored",
            "agent.task_deadline_seconds: deprecated and ignored",
        ]
    );
    let effective = BridgeConfig::load_effective(&path).unwrap();
    assert_eq!(effective.config, updated);
    assert_eq!(
        effective
            .deprecations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        diagnostics.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), legacy);
}
