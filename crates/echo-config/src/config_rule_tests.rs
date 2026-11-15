use crate::{BridgeConfig, LiveMode, McpAuth};

const EXAMPLE: &str = include_str!("../../../examples/bridge.example.toml");

fn error(input: &str, field: &str) -> String {
    let err = BridgeConfig::from_toml_str(input).unwrap_err();
    let text = err.to_string();
    assert!(text.starts_with(&format!("{field}:")), "{text}");
    for rendered in [text.clone(), format!("{err:?}")] {
        assert!(!rendered.contains("SYNTHETIC_VALUE"), "{rendered}");
        assert!(!rendered.contains(" = "), "{rendered}");
    }
    assert!(std::error::Error::source(&err).is_none());
    text
}

fn change(key: &str, old: &str, new: &str) -> String {
    EXAMPLE.replace(&format!("{key} = {old}"), &format!("{key} = {new}"))
}

#[test]
fn wyoming_port_nonzero_and_u16_boundaries() {
    for value in ["0", "-1", "65536", "\"SYNTHETIC_VALUE\""] {
        error(&change("wyoming_port", "16700", value), "echo.wyoming_port");
    }
    for value in [1, 65535] {
        let config = BridgeConfig::from_toml_str(&change("wyoming_port", "16700", &value.to_string())).unwrap();
        assert_eq!(config.echo.wyoming_port, value);
    }
}

#[test]
fn tool_cap_inclusive_limits_and_types() {
    for value in ["0", "33", "-1", "4294967296", "\"SYNTHETIC_VALUE\""] {
        error(&change("max_tool_calls", "8", value), "agent.max_tool_calls");
    }
    for value in [1, 32] {
        let config = BridgeConfig::from_toml_str(&change("max_tool_calls", "8", &value.to_string())).unwrap();
        assert_eq!(config.agent.max_tool_calls, value);
    }
}

#[test]
fn deadline_inclusive_limits_and_types() {
    for value in ["4", "601", "-1", "18446744073709551616", "\"SYNTHETIC_VALUE\""] {
        let field = "agent.task_deadline_seconds";
        error(&change("task_deadline_seconds", "60", value), field);
    }
    for value in [5, 600] {
        let config = BridgeConfig::from_toml_str(&change("task_deadline_seconds", "60", &value.to_string())).unwrap();
        assert_eq!(config.agent.task_deadline_seconds, value);
    }
}

fn extra_server(name: &str, auth: &str, extra: &str) -> String {
    format!(
        "{EXAMPLE}\n[[mcp.server]]\nname = '{name}'\nurl = 'https://mcp.example.invalid/mcp'\nauth = '{auth}'\n{extra}"
    )
}

#[test]
fn server_names_are_unique_and_ordered() {
    error(&extra_server("local-test", "none", ""), "mcp.server[1].name");
    let config = BridgeConfig::from_toml_str(&extra_server("second", "none", "")).unwrap();
    assert_eq!(
        config.mcp.server.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["local-test", "second"]
    );
    assert_eq!(config.mcp.server[1].allow_tools, Vec::<String>::new());
}

#[test]
fn http_urls_validate_authority_and_disallow_inline_credentials() {
    for url in [
        "http://127.0.0.1:18080/mcp",
        "https://mcp.example.invalid/mcp",
        "HTTP://host",
        "https://[::1]:443/mcp",
        "http://[::1]",
        "http://host?x=1#part",
    ] {
        let input = change("url", "\"http://127.0.0.1:18080/mcp\"", &format!("'{url}'"));
        assert_eq!(BridgeConfig::from_toml_str(&input).unwrap().mcp.server[0].url, url);
    }
    for url in [
        "ftp://host",
        "/mcp",
        "https:///mcp",
        "http://",
        "http://bad host",
        "http://host\\path",
        "http://user:pass@host",
        "https://[bad]",
        "http://[::1",
        "http://host:bad",
        "http://host:",
        "http://host:65536",
        "http://[::1]junk",
        "http://ho%st",
        "http://host:80:90",
    ] {
        error(
            &change("url", "\"http://127.0.0.1:18080/mcp\"", &format!("'{url}'")),
            "mcp.server[0].url",
        );
    }
}

#[test]
fn bearer_auth_requires_a_path_and_other_modes_do_not() {
    error(&extra_server("second", "bearer_file", ""), "mcp.server[1].bearer_file");
    for (auth, expected, extra) in [
        ("none", McpAuth::None, ""),
        ("portos_device_code", McpAuth::PortosDeviceCode, ""),
        ("bearer_file", McpAuth::BearerFile, "bearer_file = 'absent/bearer.txt'"),
    ] {
        let config = BridgeConfig::from_toml_str(&extra_server("second", auth, extra)).unwrap();
        assert_eq!(config.mcp.server[1].auth, expected);
        assert_eq!(
            config.mcp.server[1].bearer_file.as_deref(),
            (auth == "bearer_file").then(|| std::path::Path::new("absent/bearer.txt"))
        );
    }
}

#[test]
fn forbidden_keys_are_rejected_recursively_before_unknown_fields() {
    for key in ["token", "secret", "password", "api_key"] {
        for value in ["'SYNTHETIC_VALUE'", "''", "[]", "{}", "0", "false"] {
            for (prefix, field) in [
                ("", key.to_string()),
                ("[live]\n", format!("live.{key}")),
                ("[[unknown.items]]\n", key.to_string()),
            ] {
                let text = error(&format!("{prefix}{key} = {value}"), &field);
                assert!(text.ends_with("secrets go in files, not config"));
            }
        }
    }
    let input = change("mode", "\"chatgpt\"", "'api_key'");
    assert_eq!(BridgeConfig::from_toml_str(&input).unwrap().live.mode, LiveMode::ApiKey);
}

#[test]
fn missing_fields_unknown_fields_and_invalid_types_name_the_context() {
    for (line, field) in [
        ("host = \"echo.example.invalid\"\n", "echo.host"),
        ("credential_file = \"credentials/live.json\"\n", "live.credential_file"),
        ("name = \"local-test\"\n", "mcp.server[0].name"),
        ("auth = \"none\"\n", "mcp.server[0].auth"),
        (
            "pairing_token_file = \"credentials/screen-pairing.txt\"\n",
            "screen.pairing_token_file",
        ),
        ("log_dir = \"logs/audit\"\n", "audit.log_dir"),
    ] {
        error(&EXAMPLE.replace(line, ""), field);
    }
    for section in ["echo", "live", "agent", "mcp", "screen", "audit"] {
        let mut table = EXAMPLE.parse::<toml::Table>().unwrap();
        table.remove(section);
        error(&toml::to_string(&table).unwrap(), section);
        table.insert(section.into(), toml::Value::Boolean(false));
        error(&toml::to_string(&table).unwrap(), section);
    }
    for (key, old, new, field) in [
        ("mode", "\"chatgpt\"", "'SYNTHETIC_VALUE'", "live.mode"),
        ("auth", "\"none\"", "'SYNTHETIC_VALUE'", "mcp.server[0].auth"),
        ("voice", "\"default\"", "false", "live.voice"),
        ("allow_tools", "[\"test.echo\"]", "[false]", "mcp.server[0].allow_tools"),
        (
            "credential_file",
            "\"credentials/live.json\"",
            "false",
            "live.credential_file",
        ),
    ] {
        error(&change(key, old, new), field);
    }
    error(&format!("SYNTHETIC_VALUE = true\n{EXAMPLE}"), "config");
    for section in ["echo", "live", "agent", "mcp", "screen", "audit"] {
        let mut table = EXAMPLE.parse::<toml::Table>().unwrap();
        table
            .get_mut(section)
            .unwrap()
            .as_table_mut()
            .unwrap()
            .insert("SYNTHETIC_VALUE".into(), false.into());
        error(&toml::to_string(&table).unwrap(), section);
    }
    error(&extra_server("second", "none", "unknown = true"), "mcp.server[1]");
    error(
        &EXAMPLE.replace("[[mcp.server]]", "[mcp]\nserver = [false]\n[unknown]"),
        "mcp.server[0]",
    );
}

#[test]
fn socket_addresses_are_parsed_without_binding() {
    for value in ["127.0.0.1:1", "[::1]:16900", "0.0.0.0:0"] {
        let config =
            BridgeConfig::from_toml_str(&change("listen", "\"0.0.0.0:16900\"", &format!("'{value}'"))).unwrap();
        assert_eq!(config.screen.listen.to_string(), value);
    }
    for value in ["'SYNTHETIC_VALUE'", "false", "'host.invalid:16900'"] {
        error(&change("listen", "\"0.0.0.0:16900\"", value), "screen.listen");
    }
}

#[test]
fn syntax_and_value_errors_never_render_input() {
    for input in ["host = 'SYNTHETIC_VALUE", "[SYNTHETIC_VALUE"] {
        assert!(error(input, "config").contains("invalid TOML syntax at byte "));
    }
    error(&change("host", "\"echo.example.invalid\"", "false"), "echo.host");
}

#[test]
fn unsigned_idle_and_daily_values_have_no_invented_semantic_range() {
    // Decode through the TOML deserialization tree to preserve the full u64 range.
    for (key, old, field) in [
        ("session_idle_seconds", "120", "live.session_idle_seconds"),
        ("daily_request_cap", "500", "agent.daily_request_cap"),
    ] {
        for value in [0_u64, u64::MAX] {
            let config = BridgeConfig::from_toml_str(&change(key, old, &value.to_string())).unwrap();
            let actual = if key == "daily_request_cap" {
                config.agent.daily_request_cap
            } else {
                config.live.session_idle_seconds
            };
            assert_eq!(actual, value);
        }
        for value in ["-1", "false", "'SYNTHETIC_VALUE'"] {
            error(&change(key, old, value), field);
        }
        error(&change(key, old, "18446744073709551616"), field);
    }
}
