use crate::{BridgeConfig, LiveMode, McpAuth};

const EXAMPLE: &str = include_str!("../../../examples/bridge.example.toml");

#[test]
fn trace_payload_rejects_invalid_values_and_unknown_keys_safely() {
    for value in ["'SYNTHETIC_VALUE'", "1", "1.0", "[]", "{}"] {
        let input = EXAMPLE.replace("payloads = true", &format!("payloads = {value}"));
        assert_eq!(error(&input, "trace.payloads"), "trace.payloads: invalid type or value");
    }
    let input = EXAMPLE.replace("[trace]", "[trace]\nSYNTHETIC_VALUE=true");
    assert_eq!(error(&input, "trace"), "trace: unknown field");
    let input = EXAMPLE.replace("[trace]\npayloads = true", "");
    assert_eq!(
        error(&format!("trace='SYNTHETIC_VALUE'\n{input}"), "trace"),
        "trace: expected table"
    );
}

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
fn agent_tools_selection_defaults_and_independent_groups_are_offline() {
    use crate::AgentTools;
    assert_eq!(BridgeConfig::from_toml_str(EXAMPLE).unwrap().home, None);
    let home = std::env::temp_dir().join(format!("echo-agent-tools-uncreated-{}", std::process::id()));
    assert!(!home.exists());
    for (selection, notes, prompts) in [
        ("", true, true),
        ("agent_tools=true", true, true),
        ("agent_tools=false", false, false),
        ("agent_tools=[]", false, false),
        ("agent_tools=['notes']", true, false),
        ("agent_tools=['prompts']", false, true),
        ("agent_tools=['notes','prompts']", true, true),
        ("agent_tools=['prompts','notes']", true, true),
    ] {
        let input = format!("{EXAMPLE}\n[home]\ndir='{}'\n{selection}\n", home.display());
        let parsed = BridgeConfig::from_toml_str(&input).unwrap().home.unwrap();
        assert_eq!(parsed.dir, home);
        assert_eq!(parsed.agent_tools, AgentTools { notes, prompts });
        assert!(!home.exists());
    }
}

#[test]
fn agent_tools_rejects_duplicates_and_invalid_types_without_input_leaks() {
    let home = std::env::temp_dir();
    for (value, reason) in [
        ("['notes','notes']", "duplicate group"),
        ("['prompts','prompts']", "duplicate group"),
        ("['notes','prompts','notes']", "duplicate group"),
        ("['SYNTHETIC_VALUE']", "invalid type or value"),
        ("['Notes']", "invalid type or value"),
        ("['notes',true]", "invalid type or value"),
        ("[false]", "invalid type or value"),
        ("[1]", "invalid type or value"),
        ("[{}]", "invalid type or value"),
        ("'SYNTHETIC_VALUE'", "invalid type or value"),
        ("1", "invalid type or value"),
        ("1.0", "invalid type or value"),
        ("{}", "invalid type or value"),
    ] {
        let input = format!("{EXAMPLE}\n[home]\ndir='{}'\nagent_tools={value}\n", home.display());
        assert_eq!(error(&input, "home.agent_tools"), format!("home.agent_tools: {reason}"));
    }
}

/// The example without its own three-line `[audio]` table.
fn plain() -> String {
    let (before, table) = EXAMPLE.split_once("[audio]\n").unwrap();
    format!("{before}{}", table.splitn(3, '\n').nth(2).unwrap())
}

#[test]
fn echo_cancel_defaults_on_switches_off_and_bounds_its_hint() {
    let plain = plain();
    let audio = |table: &str| BridgeConfig::from_toml_str(&format!("{plain}\n{table}")).map(|config| config.audio);
    // The example names the defaults; an absent or empty table gives the same.
    let shown = BridgeConfig::from_toml_str(EXAMPLE).unwrap().audio;
    assert_eq!(
        (shown.echo_cancel, shown.echo_delay_ms, shown.echo_delay()),
        (true, 240, Some(240))
    );
    assert_eq!(audio("").unwrap(), shown);
    assert_eq!(audio("[audio]\n").unwrap(), shown);
    let off = audio("[audio]\necho_cancel=false\necho_delay_ms=300\n").unwrap();
    assert_eq!((off.echo_delay_ms, off.echo_delay()), (300, None));
    for value in [0, 320, 800] {
        let read = audio(&format!("[audio]\necho_delay_ms={value}\n")).unwrap();
        assert_eq!(read.echo_delay(), Some(value));
    }
    for value in ["801", "65535", "65536", "-1", "1.5", "true", "'SYNTHETIC_VALUE'"] {
        error(
            &format!("{plain}\n[audio]\necho_delay_ms={value}\n"),
            "audio.echo_delay_ms",
        );
    }
    for value in ["1", "'true'", "0.0"] {
        error(&format!("{plain}\n[audio]\necho_cancel={value}\n"), "audio.echo_cancel");
    }
}

#[test]
fn audio_lead_defaults_boundaries_and_safe_errors() {
    let plain = plain();
    for suffix in ["", "\n[audio]\n"] {
        assert_eq!(
            BridgeConfig::from_toml_str(&format!("{plain}{suffix}"))
                .unwrap()
                .audio
                .reply_lead_ms,
            200
        );
    }
    for value in [20, 200, 65520] {
        let input = format!("{plain}\n[audio]\nreply_lead_ms={value}\n");
        assert_eq!(BridgeConfig::from_toml_str(&input).unwrap().audio.reply_lead_ms, value);
    }
    for value in [
        "0",
        "1",
        "201",
        "65535",
        "65536",
        "-20",
        "1.0",
        "true",
        "'SYNTHETIC_VALUE'",
    ] {
        error(
            &format!("{plain}\n[audio]\nreply_lead_ms={value}\n"),
            "audio.reply_lead_ms",
        );
    }
    error(&format!("{plain}\n[audio]\nSYNTHETIC_VALUE=1\n"), "audio");
    error(
        &format!("{plain}\n[audio]\nreply_lead_ms={{secret='SYNTHETIC_VALUE'}}\n"),
        "audio.reply_lead_ms.secret",
    );
}

#[test]
fn clock_servers_default_to_the_pool_and_reject_unsafe_names() {
    for suffix in ["", "\n[clock]\n"] {
        let clock = BridgeConfig::from_toml_str(&format!("{EXAMPLE}{suffix}"))
            .unwrap()
            .clock;
        assert_eq!((clock.servers.clone(), clock.interval_seconds), (None, 3600));
        assert_eq!(clock.effective_servers(), crate::DEFAULT_TIME_SERVERS.map(String::from));
    }
    let named = format!("{EXAMPLE}\n[clock]\nservers=['127.0.0.1:1123','time.test']\ninterval_seconds=60\n");
    let clock = BridgeConfig::from_toml_str(&named).unwrap().clock;
    assert_eq!(clock.effective_servers(), ["127.0.0.1:1123", "time.test"]);
    assert_eq!(clock.interval_seconds, 60);
    let off = BridgeConfig::from_toml_str(&format!("{EXAMPLE}\n[clock]\nservers=[]\n")).unwrap();
    assert_eq!(off.clock.effective_servers(), Vec::<String>::new());
    for value in [
        "['']",
        "['SYNTHETIC_VALUE/path']",
        "['user@SYNTHETIC_VALUE']",
        "['bad name']",
        "['127.0.0.1:0']",
        "['a','b','c','d','e','f','g','h','i']",
        "'SYNTHETIC_VALUE'",
        "[1]",
    ] {
        error(&format!("{EXAMPLE}\n[clock]\nservers={value}\n"), "clock.servers");
    }
    for value in ["59", "86401", "-1", "'SYNTHETIC_VALUE'"] {
        error(
            &format!("{EXAMPLE}\n[clock]\ninterval_seconds={value}\n"),
            "clock.interval_seconds",
        );
    }
    error(&format!("{EXAMPLE}\n[clock]\nSYNTHETIC_VALUE=1\n"), "clock");
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
        "{EXAMPLE}\n[[mcp.server]]\nname = '{name}'\nurl = 'https://mcp.example.invalid/mcp'\nauth = '{auth}'\n{extra}" // lint-rules: allow-literal configuration parser acceptance/rejection fixture
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
        "https://mcp.example.invalid/mcp", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "HTTP://host", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "https://[::1]:443/mcp",
        "http://[::1]",
        "http://host?x=1#part", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
    ] {
        let input = change("url", "\"http://127.0.0.1:18080/mcp\"", &format!("'{url}'"));
        assert_eq!(BridgeConfig::from_toml_str(&input).unwrap().mcp.server[0].url, url);
    }
    for url in [
        "ftp://host", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "/mcp",
        "https:///mcp",
        "http://",
        "http://bad host", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "http://host\\path", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "http://user:pass@host", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "https://[bad]",   // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "http://[::1",     // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "http://host:bad", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "http://host:",    // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "http://host:65536", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "http://[::1]junk", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "http://ho%st",    // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        "http://host:80:90", // lint-rules: allow-literal configuration parser acceptance/rejection fixture
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
    for value in [
        "127.0.0.1:1",
        "[::1]:16900",
        "0.0.0.0:0", // lint-rules: allow-literal configuration socket parser fixture; no binding
    ] {
        let config =
            BridgeConfig::from_toml_str(&change("listen", "\"0.0.0.0:16900\"", &format!("'{value}'"))).unwrap(); // lint-rules: allow-literal configuration parser acceptance/rejection fixture
        assert_eq!(config.screen.listen.to_string(), value);
    }
    for value in [
        "'SYNTHETIC_VALUE'",
        "false",
        "'host.invalid:16900'", // lint-rules: allow-literal configuration socket parser rejection fixture
    ] {
        error(&change("listen", "\"0.0.0.0:16900\"", value), "screen.listen"); // lint-rules: allow-literal configuration parser acceptance/rejection fixture
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

#[test]
fn model_is_optional_exact_and_has_safe_diagnostics() {
    assert_eq!(BridgeConfig::from_toml_str(EXAMPLE).unwrap().agent.model, None);
    for value in ["fixture-model", " fixture-model ", "模型"] {
        let input = EXAMPLE.replace("[agent]", &format!("[agent]\nmodel = '{value}'"));
        assert_eq!(
            BridgeConfig::from_toml_str(&input).unwrap().agent.model.as_deref(),
            Some(value)
        );
    }
    for value in ["''", "'   '", "false", "12", "['SYNTHETIC_VALUE']"] {
        let input = EXAMPLE.replace("[agent]", &format!("[agent]\nmodel = {value}"));
        assert_eq!(
            error(&input, "agent.model"),
            if value.starts_with('\'') {
                "agent.model: must be nonblank"
            } else {
                "agent.model: invalid type or value"
            }
        );
    }
}
