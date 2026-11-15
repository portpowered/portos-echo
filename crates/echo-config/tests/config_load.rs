use echo_config::{AgentConfig, AuditConfig, BridgeConfig, EchoConfig, LiveConfig, LiveMode};
use echo_config::{McpAuth, McpConfig, McpServer, ScreenConfig};
use std::{
    error::Error,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const EXAMPLE: &str = include_str!("../../../examples/bridge.example.toml");
const MINIMAL: &str = "[echo]\nhost='echo.example.invalid'\n[live]\ncredential_file='absent/live'\n[agent]\n[mcp]\n[screen]\npairing_token_file='absent/pairing'\n[audit]\nlog_dir='absent/logs'\n";

struct Directory(PathBuf);
impl Directory {
    fn new() -> Result<Self, Box<dyn Error>> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "echo-config-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    fn write(&self, bytes: impl AsRef<[u8]>) -> Result<PathBuf, Box<dyn Error>> {
        let path = self.0.join("bridge.toml");
        std::fs::write(&path, bytes)?;
        Ok(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn committed_example_loads_with_all_expected_values() {
    let expected = BridgeConfig {
        echo: EchoConfig {
            host: "echo.example.invalid".into(),
            wyoming_port: 16700,
        },
        live: LiveConfig {
            mode: LiveMode::Chatgpt,
            credential_file: "credentials/live.json".into(),
            voice: Some("default".into()),
            session_idle_seconds: 120,
        },
        agent: AgentConfig {
            max_tool_calls: 8,
            task_deadline_seconds: 60,
            daily_request_cap: 500,
            model: None,
        },
        mcp: McpConfig {
            server: vec![McpServer {
                name: "local-test".into(),
                url: "http://127.0.0.1:18080/mcp".into(),
                auth: McpAuth::None,
                bearer_file: None,
                allow_tools: vec!["test.echo".into()],
            }],
        },
        screen: ScreenConfig {
            listen: "0.0.0.0:16900".parse().unwrap(), // lint-rules: allow-literal configuration loader expected value fixture
            pairing_token_file: "credentials/screen-pairing.txt".into(),
        },
        audit: AuditConfig {
            log_dir: "logs/audit".into(),
        },
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/bridge.example.toml");
    assert_eq!(BridgeConfig::load(path).unwrap(), expected);
    assert_eq!(BridgeConfig::from_toml_str(EXAMPLE).unwrap(), expected);
}

#[test]
fn minimal_config_defaults_match_both_loaders() {
    let directory = Directory::new().unwrap();
    let config = BridgeConfig::load(directory.write(MINIMAL).unwrap()).unwrap();
    assert_eq!(config, BridgeConfig::from_toml_str(MINIMAL).unwrap());
    assert_eq!(config.echo.wyoming_port, 16700);
    assert_eq!(config.live.mode, LiveMode::Chatgpt);
    assert_eq!(config.live.voice, None);
    assert_eq!(config.live.session_idle_seconds, 120);
    assert_eq!(
        config.agent,
        AgentConfig {
            max_tool_calls: 8,
            task_deadline_seconds: 60,
            daily_request_cap: 500,
            model: None,
        }
    );
    assert_eq!(config.mcp.server, vec![]);
    assert_eq!(config.screen.listen.to_string(), "0.0.0.0:16900"); // lint-rules: allow-literal configuration loader expected value fixture
}

#[test]
fn all_auth_modes_and_ordered_tools_load_with_untouched_paths() {
    let input = format!(
        "{}\n[[mcp.server]]\nname='device'\nurl='https://mcp.example.invalid/mcp'\nauth='portos_device_code'\n[[mcp.server]]\nname='bearer'\nurl='http://127.0.0.1:1/mcp'\nauth='bearer_file'\nbearer_file='absent/bearer'\nallow_tools=['test.wait','test.echo']", // lint-rules: allow-literal configuration loader expected value fixture
        MINIMAL.replace("[live]", "[live]\nmode='api_key'")
    );
    let directory = Directory::new().unwrap();
    let config = BridgeConfig::load(directory.write(input).unwrap()).unwrap();
    assert_eq!(config.live.mode, LiveMode::ApiKey);
    assert_eq!(config.mcp.server[0].auth, McpAuth::PortosDeviceCode);
    assert_eq!(config.mcp.server[1].auth, McpAuth::BearerFile);
    assert_eq!(config.mcp.server[1].bearer_file, Some("absent/bearer".into()));
    assert_eq!(config.mcp.server[1].allow_tools, ["test.wait", "test.echo"]);
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    assert!(!directory.0.join("absent").exists());
}

#[test]
fn inline_api_key_is_rejected_without_exposing_input() {
    let input = EXAMPLE.replace("[live]", "[live]\napi_key = 'SYNTHETIC_VALUE'");
    let directory = Directory::new().unwrap();
    let path = directory.write(&input).unwrap();
    for result in [BridgeConfig::load(path), BridgeConfig::from_toml_str(&input)] {
        let err = result.unwrap_err();
        assert_eq!(err.to_string(), "live.api_key: secrets go in files, not config");
        assert!(!format!("{err:?}").contains("SYNTHETIC_VALUE"));
        assert!(err.source().is_none());
    }
}

#[test]
fn file_validation_returns_no_partial_configuration() {
    let directory = Directory::new().unwrap();
    let err = BridgeConfig::load(
        directory
            .write(EXAMPLE.replace("wyoming_port = 16700", "wyoming_port = 0"))
            .unwrap(),
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "echo.wyoming_port: must be nonzero");
}

#[test]
fn missing_directory_and_non_utf8_inputs_are_safe_errors() {
    let directory = Directory::new().unwrap();
    for path in [
        directory.0.join("missing"),
        directory.0.clone(),
        directory.write([0xff, 0xfe]).unwrap(),
    ] {
        let err = BridgeConfig::load(path).unwrap_err();
        assert_eq!(err.to_string(), "config: cannot read UTF-8 configuration file");
        assert!(err.source().is_none());
        assert!(!format!("{err:?}").contains("echo-config-"));
    }
}

#[test]
fn malformed_missing_unknown_and_wrongly_typed_inputs_are_safe() {
    let directory = Directory::new().unwrap();
    for (input, field) in [
        ("x = 'SYNTHETIC_VALUE", "config"),
        ("", "echo"),
        ("SYNTHETIC_VALUE=true", "echo"),
    ] {
        let err = BridgeConfig::load(directory.write(input).unwrap()).unwrap_err();
        assert!(err.to_string().starts_with(&format!("{field}:")));
        assert!(!format!("{err:?}").contains("SYNTHETIC_VALUE"));
    }
}

#[test]
fn repeated_and_parallel_loads_retain_literal_paths_without_writes() {
    let directory = Directory::new().unwrap();
    let path = directory
        .write(MINIMAL.replace("absent/live", "~/absent/$CONFIG_TEST/live"))
        .unwrap();
    let initial = BridgeConfig::load(&path).unwrap();
    assert_eq!(
        initial.live.credential_file,
        PathBuf::from("~/absent/$CONFIG_TEST/live")
    );
    assert_eq!(initial.screen.pairing_token_file, PathBuf::from("absent/pairing"));
    assert_eq!(initial.audit.log_dir, PathBuf::from("absent/logs"));
    std::thread::scope(|scope| {
        let other = scope.spawn(|| BridgeConfig::load(&path).unwrap());
        assert_eq!(BridgeConfig::load(&path).unwrap(), initial);
        assert_eq!(other.join().unwrap(), initial);
    });
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        MINIMAL.replace("absent/live", "~/absent/$CONFIG_TEST/live")
    );
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn nested_secret_diagnostics_use_only_trusted_schema_context() {
    let directory = Directory::new().unwrap();
    for key in ["SYNTHETIC_VALUE", r#""SYNTHETIC_VALUE\nFORGED_LINE""#] {
        for secret in ["token", "secret", "password", "api_key"] {
            let nested = format!("{{ {key} = [{{ {secret} = 'DISCARDED_VALUE' }}] }}");
            for (input, field) in [
                (
                    EXAMPLE.replace("\"credentials/live.json\"", &nested),
                    "live.credential_file",
                ),
                (EXAMPLE.replace("[live]", &format!("[live]\n{key} = {nested}")), "live"),
                (format!("{key} = {nested}\n{EXAMPLE}"), ""),
                (
                    EXAMPLE.replace("allow_tools = [\"test.echo\"]", &format!("allow_tools = [{nested}]")),
                    "mcp.server[0].allow_tools",
                ),
                (
                    EXAMPLE.replace("[[mcp.server]]", &format!("[[mcp.server]]\n{key} = {nested}")),
                    "mcp.server[0]",
                ),
            ] {
                let path = directory.write(&input).unwrap();
                for result in [BridgeConfig::from_toml_str(&input), BridgeConfig::load(&path)] {
                    let err = result.unwrap_err();
                    let expected_field = if field.is_empty() {
                        secret.into()
                    } else {
                        format!("{field}.{secret}")
                    };
                    assert_eq!(
                        err.to_string(),
                        format!("{expected_field}: secrets go in files, not config")
                    );
                    for rendered in [err.to_string(), format!("{err:?}")] {
                        for untrusted in ["SYNTHETIC_VALUE", "FORGED_LINE", "DISCARDED_VALUE", "\n"] {
                            assert!(!rendered.contains(untrusted), "{rendered}");
                        }
                    }
                    assert!(err.source().is_none());
                }
            }
        }
    }
}
