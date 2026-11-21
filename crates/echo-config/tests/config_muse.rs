use echo_config::BridgeConfig;
use std::path::Path;

const EXAMPLE: &str = include_str!("../../../examples/bridge.example.toml");
const MUSE: &str = "\n[muse]\nurl='https://127.0.0.1:7448'\nca_file='absent/ca.der'\ncredential_file='~/absent/muse'\n";

fn selected(backend: &str) -> String {
    EXAMPLE.replace("[agent]", &format!("[agent]\nbackend={backend}"))
}

fn error(input: &str, expected: &str) {
    let result = BridgeConfig::from_toml_str(input);
    assert!(result.is_err());
    if let Err(err) = result {
        assert_eq!(err.to_string(), expected);
        assert!(!format!("{err:?}").contains("SYNTHETIC_VALUE"));
        assert!(std::error::Error::source(&err).is_none());
    }
}

#[test]
fn muse_configuration_is_optional_and_does_not_open_its_files() {
    let default = BridgeConfig::from_toml_str(EXAMPLE).unwrap();
    assert_eq!(default.muse, None);
    let config = BridgeConfig::from_toml_str(&format!("{EXAMPLE}{MUSE}")).unwrap();
    assert_eq!(config.agent, default.agent);
    let muse = config.muse.unwrap();
    assert_eq!(muse.url, "https://127.0.0.1:7448");
    assert_eq!(muse.ca_file, Path::new("absent/ca.der"));
    assert_eq!(muse.credential_file, Path::new("~/absent/muse"));
}

#[test]
fn retired_backend_has_one_migration_diagnostic() {
    for value in [
        "'client'",
        "'muse'",
        "'SYNTHETIC_VALUE'",
        "true",
        "12",
        "[]",
        "{token='SYNTHETIC_VALUE'}",
    ] {
        error(&selected(value), "agent.backend: removed; configure Muse with [muse]");
    }
}

#[test]
fn muse_url_accepts_only_https_origins_with_nonzero_ports() {
    for url in [
        "https://127.0.0.1:1",
        "https://127.0.0.1:65535/",
        "https://[::1]:443",
        "https://muse.example.invalid", // lint-rules: allow-literal offline Muse origin parser fixture; never dialed
        "https://muse.example.invalid/", // lint-rules: allow-literal offline Muse origin validation fixture
    ] {
        let input = format!("{EXAMPLE}{}", MUSE.replace("https://127.0.0.1:7448", url));
        assert_eq!(BridgeConfig::from_toml_str(&input).unwrap().muse.unwrap().url, url);
    }
    for url in [
        "",
        "http://127.0.0.1:7448",
        "https:///",
        "https://127.0.0.1:0",
        "https://127.0.0.1:65536", // lint-rules: allow-literal offline Muse origin parser fixture; never dialed
        "https://127.0.0.1:",
        "https://127.0.0.1:bad", // lint-rules: allow-literal offline Muse origin parser fixture; never dialed
        "https://127.0.0.1/path",
        "https://127.0.0.1//",
        "https://127.0.0.1?x",
        "https://127.0.0.1#x",
        "https://SYNTHETIC_VALUE@127.0.0.1",
        "https://[::1]:0",
        "https://[bad]", // lint-rules: allow-literal offline Muse origin parser fixture; never dialed
        "https://[::1]suffix", // lint-rules: allow-literal offline Muse origin parser fixture; never dialed
        "https://127.0.0.1\\path",
        "https://bad host", // lint-rules: allow-literal offline Muse origin rejection fixture
    ] {
        error(
            &format!("{EXAMPLE}{}", MUSE.replace("https://127.0.0.1:7448", url)),
            "muse.url: expected HTTPS origin without inline credentials",
        );
    }
}

#[test]
fn muse_fields_have_safe_exact_missing_type_and_unknown_diagnostics() {
    for (field, value) in [
        ("url", "https://127.0.0.1:7448"),
        ("ca_file", "absent/ca.der"),
        ("credential_file", "~/absent/muse"),
    ] {
        let line = format!("{field}='{value}'\n");
        error(
            &format!("{EXAMPLE}{}", MUSE.replace(&line, "")),
            &format!("muse.{field}: missing required field"),
        );
        error(
            &format!("{EXAMPLE}{}", MUSE.replace(&line, &format!("{field}=false\n"))),
            &format!("muse.{field}: invalid type or value"),
        );
    }
    error(&format!("{EXAMPLE}{MUSE}SYNTHETIC_VALUE=true\n"), "muse: unknown field");
    error(&format!("{EXAMPLE}muse=false\n"), "audit: unknown field");
    error(&format!("muse=false\n{EXAMPLE}"), "muse: expected table");
    for field in ["ca_file", "credential_file"] {
        for value in ["", "   "] {
            let original = if field == "ca_file" {
                "absent/ca.der"
            } else {
                "~/absent/muse"
            };
            error(
                &format!("{EXAMPLE}{}", MUSE.replace(original, value)),
                &format!("muse.{field}: expected nonempty path"),
            );
        }
    }
}

#[test]
fn muse_inline_secrets_are_rejected_before_decoding() {
    for key in ["token", "secret", "password", "api_key"] {
        error(
            &format!("{EXAMPLE}{MUSE}{key}='SYNTHETIC_VALUE'\n"),
            &format!("muse.{key}: secrets go in files, not config"),
        );
    }
}
