use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

// Public, self-signed DER fixture. The catalog only parses it and never dials.
const CERTIFICATE: &str = "MIIBkDCCATegAwIBAgIUJxLu8jX9XP/JTQUR/UI7n3goxWwwCgYIKoZIzj0EAwIwHjEcMBoGA1UEAwwTc3ludGhldGljLW11c2UtdGVzdDAeFw0yNjExMjEwMjEwMjRaFw0yNjExMjIwMjEwMjRaMB4xHDAaBgNVBAMME3N5bnRoZXRpYy1tdXNlLXRlc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAQMEC+oNl6lzdJccqQNin474Is0YkYlW+7j8zfeOXNIChXUG0v3lx/SAUQjEmfa2lzyk0NsTN2MglbngBB1ccvLo1MwUTAdBgNVHQ4EFgQU6kLVpgRF3+67GuzWXe9yV+VPLIEwHwYDVR0jBBgwFoAU6kLVpgRF3+67GuzWXe9yV+VPLIEwDwYDVR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNHADBEAiB8RC8dr6A1Dk0BpgqgWuSJtwYR61+WVI6Khf9LNrpk2AIgHwrch7210xblYW7iiURHZBfqhtwhRr3+CEy41FjWt3A=";

fn private(path: &std::path::Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[tokio::test]
async fn muse_tool_advertised_once_only_with_usable_connection_files() {
    let root = tempfile::tempdir().unwrap();
    let text = include_str!("../../../../../../examples/bridge.example.toml");
    let mut config = BridgeConfig::from_toml_str(text).unwrap();
    config.mcp.server.clear();
    let ca = root.path().join("synthetic.der");
    let credential = root.path().join("synthetic-credential");
    let mut trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    let screen = Screen::bind(None).await.unwrap();
    let connection = echo_config::MuseConfig {
        url: "https://127.0.0.1:1".into(),
        ca_file: ca.clone(),
        credential_file: credential.clone(),
    };
    private(&ca, &STANDARD.decode(CERTIFICATE).unwrap());
    private(&credential, &[b'x'; 32]);
    assert_catalog(&config, &screen, &mut trace, false).await;
    config.muse = Some(connection);
    assert_catalog(&config, &screen, &mut trace, true).await;
    private(&ca, b"invalid certificate");
    assert_catalog(&config, &screen, &mut trace, false).await;
    private(&ca, &STANDARD.decode(CERTIFICATE).unwrap());
    std::fs::remove_file(credential).unwrap();
    assert_catalog(&config, &screen, &mut trace, false).await;
    screen.close().await.unwrap();
}

async fn assert_catalog(config: &BridgeConfig, screen: &Screen, trace: &mut Trace, enabled: bool) {
    let mut tools = Tools::new(config, Arc::new(|| 0)).with_muse(config, true);
    tools.open(config, screen, trace, None).await.unwrap();
    let catalog = tools.catalog().await.unwrap();
    let names = catalog
        .tools()
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    let mut expected = vec!["extend_session", "end_session"];
    if enabled {
        expected.push("ask_muse");
        assert_eq!(
            catalog.tools().last().unwrap().input_schema,
            json!({
                "type":"object","additionalProperties":false,"required":["request"],
                "properties":{"request":{"type":"string","minLength":1}}
            })
        );
    }
    assert_eq!(names, expected);
    tools.close().await;
}

#[test]
fn muse_tool_name_is_reserved_even_when_disabled() {
    let mut collision = crate::bridge_muse::tool_spec();
    collision.name = "ask.muse".into();
    assert!(matches!(session_catalog(vec![collision], &[]), Err(Error::Input)));
}
