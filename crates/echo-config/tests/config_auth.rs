use echo_config::{BridgeConfig, ChatGptAuthConfig};
use std::fmt::Write;
const MINIMAL: &str = "[echo]\nhost='local'\n[live]\ncredential_file='missing'\n[agent]\n[mcp]\n[screen]\npairing_token_file='missing'\n[audit]\nlog_dir='missing'\n";

#[test]
fn mcp_link_defaults_and_explicit_overrides_remain_offline() {
    let old = BridgeConfig::from_toml_str(MINIMAL).unwrap().mcp;
    assert_eq!(old, echo_config::McpConfig::default());
    assert_eq!(old.state_dir, None);
    assert_eq!(old.client_id, "port1/systems/zero/oauth-clients/command-line-interface");
    assert_eq!(old.scopes, ["endpoint:read"]);
    assert_eq!(old.operator_listen.to_string(), "127.0.0.1:16902");
    let input = MINIMAL.replace("[mcp]", "[mcp]\nstate_dir='absent/mcp-state'\nclient_id='public-test-client'\nscopes=['endpoint:read','other:read']\noperator_listen='127.0.0.1:0'");
    let config = BridgeConfig::from_toml_str(&input).unwrap().mcp;
    assert_eq!(config.state_dir, Some("absent/mcp-state".into()));
    assert_eq!(config.client_id, "public-test-client");
    assert_eq!(config.scopes, ["endpoint:read", "other:read"]);
    assert_eq!(config.operator_listen.to_string(), "127.0.0.1:0");
    // A legacy device-code config still decodes without creating a grant store.
    let legacy =
        format!("{MINIMAL}\n[[mcp.server]]\nname='portos'\nurl='http://127.0.0.1:1/mcp'\nauth='portos_device_code'");
    let config = BridgeConfig::from_toml_str(&legacy).unwrap().mcp;
    assert_eq!(config.state_dir, None);
    assert_eq!(config.server[0].auth, echo_config::McpAuth::PortosDeviceCode);
}

#[test]
fn mcp_link_rejects_invalid_fields_without_echoing_values() {
    for (field, input, message) in [
        ("client_id", "''", "expected nonblank text"),
        ("client_id", "'  '", "expected nonblank text"),
        ("client_id", "\"private\\nvalue\"", "expected nonblank text"),
        ("scopes", "[]", "expected nonempty scope names"),
        ("scopes", "['']", "expected nonempty scope names"),
        (
            "scopes",
            "['endpoint:read other:read']",
            "expected nonempty scope names",
        ),
        ("scopes", "[1]", "invalid type or value"),
        ("state_dir", "''", "expected nonempty path"),
        ("state_dir", "4", "invalid type or value"),
        ("operator_listen", "'localhost:1'", "expected socket address"),
        ("operator_listen", "'0.0.0.0:1'", "expected literal IPv4 loopback"), // lint-rules: allow-literal rejected offline configuration
        ("operator_listen", "'[::1]:1'", "expected literal IPv4 loopback"), // lint-rules: allow-literal rejected offline configuration
        ("operator_listen", "'127.0.0.2:1'", "expected literal IPv4 loopback"), // lint-rules: allow-literal rejected offline configuration
    ] {
        let input = MINIMAL.replace("[mcp]", &format!("[mcp]\n{field}={input}"));
        assert_eq!(
            BridgeConfig::from_toml_str(&input).unwrap_err().to_string(),
            format!("mcp.{field}: {message}")
        );
    }
    for field in ["state_dir", "client_id", "scopes", "operator_listen"] {
        let input = MINIMAL.replace("[mcp]", &format!("[mcp]\n{field}={{secret='private'}}"));
        assert_eq!(
            BridgeConfig::from_toml_str(&input).unwrap_err().to_string(),
            format!("mcp.{field}.secret: secrets go in files, not config")
        );
    }
}

#[test]
fn auth_defaults_and_every_override_are_offline() {
    let old = BridgeConfig::from_toml_str(MINIMAL).unwrap();
    let d = old.chatgpt_auth;
    assert_eq!(d, ChatGptAuthConfig::default());
    assert_eq!(d.client_id, "app_EMoamEEZ73f0CkXaXp7hrann");
    assert_eq!(d.scopes, "openid profile email offline_access");
    assert_eq!(d.issuer, "https://auth.openai.com"); // lint-rules: allow-literal inert defaults assertion
    let fields = [
        "issuer",
        "usercode_endpoint",
        "device_token_endpoint",
        "token_endpoint",
        "authorization_endpoint",
        "verification_url",
        "device_redirect_uri",
    ];
    let expected = [
        "",
        "/api/accounts/deviceauth/usercode",
        "/api/accounts/deviceauth/token",
        "/oauth/token",
        "/oauth/authorize",
        "/codex/device",
        "/deviceauth/callback",
    ];
    let values = [
        &d.issuer,
        &d.usercode_endpoint,
        &d.device_token_endpoint,
        &d.token_endpoint,
        &d.authorization_endpoint,
        &d.verification_url,
        &d.device_redirect_uri,
    ];
    for (value, suffix) in values.iter().zip(expected) {
        assert_eq!(**value, format!("{}{suffix}", d.issuer));
    }
    assert_eq!(d.browser_redirect_uri, "http://localhost:1455/auth/callback");
    let mut input = format!(
        "{MINIMAL}\n[chatgpt_auth]\nclient_id='mock-client'\nscopes='mock scopes'\nbrowser_redirect_uri='http://127.0.0.1:1234/auth/callback'\n"
    );
    for field in fields {
        writeln!(input, "{field}='http://127.0.0.1:1234/{field}'").unwrap();
    }
    let config = BridgeConfig::from_toml_str(&input).unwrap().chatgpt_auth;
    assert_eq!(config.client_id, "mock-client");
    assert_eq!(config.scopes, "mock scopes");
    assert_eq!(config.browser_redirect_uri, "http://127.0.0.1:1234/auth/callback");
    let values = [
        config.issuer,
        config.usercode_endpoint,
        config.device_token_endpoint,
        config.token_endpoint,
        config.authorization_endpoint,
        config.verification_url,
        config.device_redirect_uri,
    ];
    for (field, value) in fields.iter().zip(values) {
        assert_eq!(value, format!("http://127.0.0.1:1234/{field}"));
    }
}

#[test]
fn auth_rejects_unsafe_values_with_field_only_errors() {
    let cases = [
        ("client_id=''", "client_id"),
        ("scopes='  '", "scopes"),
        ("issuer=3", "issuer"),
        ("token_endpoint='http://remote.invalid/token'", "token_endpoint"), // lint-rules: allow-literal inert invalid configuration fixture
        (
            "usercode_endpoint='https://user:pass@example.invalid/x'", // lint-rules: allow-literal inert invalid configuration fixture
            "usercode_endpoint",
        ),
        (
            "verification_url='https://example.invalid/x#fragment'", // lint-rules: allow-literal inert invalid configuration fixture
            "verification_url",
        ),
        ("device_redirect_uri='file:///tmp/auth'", "device_redirect_uri"), // lint-rules: allow-literal inert invalid configuration fixture
        (
            "browser_redirect_uri='http://0.0.0.0:1234/auth/callback'", // lint-rules: allow-literal inert invalid configuration fixture
            "browser_redirect_uri",
        ),
        (
            "browser_redirect_uri='http://[::1]:1234/auth/callback'", // lint-rules: allow-literal inert invalid configuration fixture
            "browser_redirect_uri",
        ),
        (
            "browser_redirect_uri='http://127.0.0.1:1234/wrong'",
            "browser_redirect_uri",
        ),
    ];
    for (input, field) in cases {
        let err = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n[chatgpt_auth]\n{input}")).unwrap_err();
        assert!(err.to_string().starts_with(&format!("chatgpt_auth.{field}:")), "{err}");
        assert!(!err.to_string().contains("example.invalid"));
    }
    for (input, expected) in [
        ("unexpected='VALUE'", "chatgpt_auth: unknown field"),
        ("token='VALUE'", "chatgpt_auth.token: secrets go in files, not config"),
        (
            "client_id={secret='VALUE'}",
            "chatgpt_auth.client_id.secret: secrets go in files, not config",
        ),
    ] {
        assert_eq!(
            BridgeConfig::from_toml_str(&format!("{MINIMAL}\n[chatgpt_auth]\n{input}"))
                .unwrap_err()
                .to_string(),
            expected
        );
    }
    assert_eq!(
        BridgeConfig::from_toml_str(&format!("chatgpt_auth=1\n{MINIMAL}"))
            .unwrap_err()
            .to_string(),
        "chatgpt_auth: expected table"
    );
}

#[test]
fn auth_loader_rejects_prefixed_callback_paths() {
    for host in ["localhost", "127.0.0.1"] {
        let input = format!("{MINIMAL}\n[chatgpt_auth]\nbrowser_redirect_uri='http://{host}:0/prefix/auth/callback'\n"); // lint-rules: allow-literal inert rejected callback configuration, never requested
        assert_eq!(
            BridgeConfig::from_toml_str(&input).unwrap_err().to_string(),
            "chatgpt_auth.browser_redirect_uri: expected loopback HTTP /auth/callback"
        );
    }
}

#[test]
fn auth_endpoint_and_redirect_boundaries_keep_safe_diagnostics() {
    let mut config = ChatGptAuthConfig {
        token_endpoint: "http://127.0.0.1/token".into(),
        ..ChatGptAuthConfig::default()
    };
    assert_eq!(config.validate().map_err(|error| error.to_string()), Ok(()));
    for endpoint in [
        "invalid",
        "ftp://127.0.0.1/token",
        "https://127.0.0.1/token?secret=hidden",
    ] {
        // lint-rules: allow-literal rejected endpoint inputs, never requested
        config.token_endpoint = endpoint.into();
        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "chatgpt_auth.token_endpoint: expected HTTPS or literal loopback HTTP URL"
        );
    }
    config = ChatGptAuthConfig::default();
    config.client_id = "mock\u{7f}client".into();
    assert_eq!(
        config.validate().unwrap_err().to_string(),
        "chatgpt_auth.client_id: expected nonempty text"
    );
    config = ChatGptAuthConfig::default();
    for redirect in [
        "invalid",
        "https://127.0.0.1/auth/callback",
        "http://127.0.0.1/auth/callback?next=/auth/callback",
        "http://127.0.0.1/auth/callback#fragment/auth/callback",
    ] {
        // lint-rules: allow-literal rejected redirect inputs, never requested
        config.browser_redirect_uri = redirect.into();
        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "chatgpt_auth.browser_redirect_uri: expected loopback HTTP /auth/callback"
        );
    }
}
