use super::{BridgeConfig, MINIMAL, overlay};
use echo_config::SettingsOverlay;

#[test]
fn endpoint_overlay_keeps_operator_origins_and_sparse_siblings() {
    let base = BridgeConfig::from_toml_str(&format!(
        "{MINIMAL}\n[portos_endpoint]\ngateway_url='wss://gateway.example.invalid/devices/{{authLinkId}}'\n\
         api_base='https://api.example.invalid'"
    ))
    .unwrap();
    assert!(!base.portos_endpoint.enabled);
    assert_eq!(base.portos_endpoint.device_name, "Echo Show");

    let saved = overlay("[portos_endpoint]\ndevice_name='Hall Echo'\n[voice]\nsession_listen_enabled=false\n");
    let saved = saved.merge(&overlay("[portos_endpoint]\nenabled=true"));
    let persisted = saved.to_toml_string().unwrap();
    let effective = overlay(&persisted).apply_to(&base).unwrap();
    assert!(effective.portos_endpoint.enabled);
    assert_eq!(effective.portos_endpoint.device_name, "Hall Echo");
    assert_eq!(effective.portos_endpoint.gateway_url, base.portos_endpoint.gateway_url);
    assert_eq!(effective.portos_endpoint.api_base, base.portos_endpoint.api_base);
    assert!(!effective.voice.session_listen_enabled);
    assert!(!persisted.contains("gateway_url"));
    assert!(!persisted.contains("api_base"));
}

#[test]
fn endpoint_names_use_the_same_bound_and_safe_diagnostic() {
    for (name, valid) in [
        (String::new(), false),
        (" \u{2003}".into(), false),
        ("Hall\\tEcho".into(), false),
        ("界".repeat(65), false),
        ("界".repeat(64), true),
    ] {
        let section = format!("[portos_endpoint]\ndevice_name=\"{name}\"\n");
        let base = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n{section}"));
        let settings = SettingsOverlay::from_toml_str(&section);
        if valid {
            let original = BridgeConfig::from_toml_str(MINIMAL).unwrap();
            assert_eq!(base.unwrap().portos_endpoint.device_name, name);
            assert_eq!(
                settings
                    .unwrap()
                    .apply_to(&original)
                    .unwrap()
                    .portos_endpoint
                    .device_name,
                name
            );
        } else {
            let diagnostic = "portos_endpoint.device_name: expected 1 to 64 characters, nonblank and without controls";
            assert_eq!(base.unwrap_err().to_string(), diagnostic);
            assert_eq!(settings.unwrap_err().to_string(), diagnostic);
        }
    }
}

#[test]
fn endpoint_origin_configuration_refuses_unsafe_and_noneditable_values() {
    for (field, value) in [
        ("gateway_url", "ws://127.0.0.1/devices/{authLinkId}"),
        (
            "gateway_url",
            "wss://gateway.example.invalid/{other}/devices/{authLinkId}",
        ),
        (
            "gateway_url",
            "wss://gateway.example.invalid/devices/{authLinkId}?extra",
        ),
        ("gateway_url", "wss://user@gateway.example.invalid/devices/{authLinkId}"),
        ("api_base", "http://api.example.invalid"),
        ("api_base", "https://api.example.invalid#fragment"),
        ("api_base", "https://api.example.invalid/{other}"),
    ] {
        let section = format!("[portos_endpoint]\n{field}='{value}'");
        let error = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n{section}")).unwrap_err();
        assert!(error.to_string().starts_with(&format!("portos_endpoint.{field}: ")));
        assert!(!error.to_string().contains(value));
        assert_eq!(
            SettingsOverlay::from_toml_str(&section).unwrap_err().to_string(),
            "portos_endpoint: unknown field"
        );
    }
}
