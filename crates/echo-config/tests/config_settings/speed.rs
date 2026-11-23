use super::{BridgeConfig, MINIMAL, overlay};
use echo_config::{DelegateSpeed, SettingsOverlay};

#[test]
fn screen_settings_speed_preserves_inheritance_and_explicit_standard() {
    let base = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n[codex]\ndelegate_speed='ultrafast'")).unwrap();
    let other = overlay("[codex]\ndelegate_effort='low'");
    assert_eq!(
        other.apply_to(&base).unwrap().codex.delegate_speed,
        DelegateSpeed::Ultrafast
    );
    let settings = other.merge(&overlay("[codex]\ndelegate_speed='standard'"));
    let encoded = settings.to_toml_string().unwrap();
    assert_eq!(
        encoded,
        "[codex]\ndelegate_effort = \"low\"\ndelegate_speed = \"standard\"\n"
    );
    let effective = overlay(&encoded).apply_to(&base).unwrap();
    assert_eq!(effective.codex.delegate_speed, DelegateSpeed::Standard);
    assert_eq!(effective.codex.delegate_effort.as_deref(), Some("low"));
    assert_eq!(base.codex.delegate_speed, DelegateSpeed::Ultrafast);
    for speed in [DelegateSpeed::Standard, DelegateSpeed::Fast, DelegateSpeed::Ultrafast] {
        let saved = overlay(&format!("[codex]\ndelegate_speed='{}'", speed.as_str()));
        let reloaded = overlay(&saved.to_toml_string().unwrap());
        assert_eq!(reloaded, saved);
        assert_eq!(reloaded.apply_to(&base).unwrap().codex.delegate_speed, speed);
    }
}

#[test]
fn screen_settings_legacy_fast_loads_and_is_saved_as_speed() {
    let base = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n[codex]\ndelegate_fast=true")).unwrap();
    assert_eq!(base.codex.delegate_speed, DelegateSpeed::Fast);
    for (stored, expected) in [
        ("delegate_fast=false", DelegateSpeed::Standard),
        ("delegate_fast=true", DelegateSpeed::Fast),
        ("delegate_fast=true\ndelegate_speed='fast'", DelegateSpeed::Fast),
    ] {
        let legacy = overlay(&format!("[codex]\n{stored}"));
        assert_eq!(legacy.apply_to(&base).unwrap().codex.delegate_speed, expected);
        assert_eq!(
            legacy.to_toml_string().unwrap(),
            format!("[codex]\ndelegate_speed = \"{}\"\n", expected.as_str())
        );
    }
    let upgraded = overlay("[codex]\ndelegate_fast=true").merge(&overlay("[codex]\ndelegate_speed='ultrafast'"));
    assert_eq!(
        upgraded.to_toml_string().unwrap(),
        "[codex]\ndelegate_speed = \"ultrafast\"\n"
    );
    for (stored, expected) in [
        (
            "delegate_fast=false\ndelegate_speed='ultrafast'",
            "codex.delegate_speed: conflicts with codex.delegate_fast; remove delegate_fast",
        ),
        ("delegate_fast='false'", "codex.delegate_fast: invalid type or value"),
        ("delegate_speed='turbo'", "codex.delegate_speed: invalid type or value"),
    ] {
        let input = format!("[agent]\nmax_tool_calls=3\n[codex]\n{stored}");
        assert_eq!(
            SettingsOverlay::from_toml_str(&input).unwrap_err().to_string(),
            expected
        );
    }
}
