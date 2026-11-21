use super::MINIMAL;
use echo_config::BridgeConfig;

#[test]
fn camera_base_config_defaults_and_applies_documented_overrides() {
    let parse = |table| BridgeConfig::from_toml_str(&format!("{MINIMAL}\n{table}"));
    let omitted = parse("").unwrap().camera;
    assert_eq!(omitted.enabled, cfg!(target_os = "android"));
    assert_eq!(omitted.id, "0");
    let selected = parse("[camera]\nenabled = true\nid = 'rear camera'").unwrap().camera;
    assert!(selected.enabled);
    assert_eq!(selected.id, "rear camera");
    let disabled = parse("[camera]\nenabled = false").unwrap().camera;
    assert!(!disabled.enabled);
    assert_eq!(disabled.id, "0");
}

#[test]
fn camera_base_config_errors_do_not_expose_rejected_values_or_keys() {
    for (table, message) in [
        (
            "[camera]\nenabled = 'untrusted'",
            "camera.enabled: invalid type or value",
        ),
        ("[camera]\nid = 10", "camera.id: invalid type or value"),
        (
            "[camera]\nid = '  '",
            "camera.id: expected a nonblank camera ID without controls",
        ),
        (
            r#"[camera]
id = "untrusted\nvalue""#,
            "camera.id: expected a nonblank camera ID without controls",
        ),
        ("[camera]\nuntrusted = 'value'", "camera: unknown field"),
    ] {
        assert_eq!(
            BridgeConfig::from_toml_str(&format!("{MINIMAL}\n{table}"))
                .unwrap_err()
                .to_string(),
            message,
        );
    }
}
