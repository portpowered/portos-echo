use echo_config::{BridgeConfig, ChatGptAuthConfig};
use std::fmt::Write;
const MINIMAL: &str = "[echo]\nhost='local'\n[live]\ncredential_file='missing'\n[agent]\n[mcp]\n[screen]\npairing_token_file='missing'\n[audit]\nlog_dir='missing'\n";

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
