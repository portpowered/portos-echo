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
        wake_earcon: true,
        codex: echo_config::CodexConfig::default(),
        agent_backend: echo_config::AgentBackend::Client,
        muse: None,
        chatgpt_auth: echo_config::ChatGptAuthConfig::default(),
        audio: echo_config::AudioConfig::default(),
        voice: echo_config::VoiceConfig::default(),
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
            ..McpConfig::default()
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
fn wake_earcon_defaults_and_boolean_overrides_are_offline() {
    let directory = Directory::new().unwrap();
    for (prefix, expected) in [("", true), ("wake_earcon=true\n", true), ("wake_earcon=false\n", false)] {
        let input = format!("{prefix}{MINIMAL}");
        let parsed = BridgeConfig::from_toml_str(&input).unwrap();
        assert_eq!(parsed.wake_earcon, expected);
        assert_eq!(BridgeConfig::load(directory.write(input).unwrap()).unwrap(), parsed);
    }
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn voice_listen_defaults_and_overrides_match_both_offline_loaders() {
    use echo_config::VoiceConfig;
    let directory = Directory::new().unwrap();
    let credential = directory.0.join("unopened-credential-directory");
    std::fs::create_dir(&credential).unwrap();
    let minimal = MINIMAL.replace("absent/live", &credential.to_string_lossy().replace('\\', "/"));
    for (fields, listen, max) in [
        ("", 15, 600),
        ("[voice]\n", 15, 600),
        ("[voice]\nsession_listen_seconds=3\n", 3, 600),
        ("[voice]\nsession_listen_seconds=3\nsession_max_seconds=3\n", 3, 3),
        ("[voice]\nsession_max_seconds=15\n", 15, 15),
        (
            "[voice]\nsession_listen_seconds=300\nsession_max_seconds=2147483647\n",
            300,
            2_147_483_647,
        ),
    ] {
        let input = format!("{minimal}\n{fields}");
        let parsed = BridgeConfig::from_toml_str(&input).unwrap();
        assert_eq!(BridgeConfig::load(directory.write(input).unwrap()).unwrap(), parsed);
        assert_eq!(
            parsed.voice,
            VoiceConfig {
                session_listen_seconds: listen,
                session_max_seconds: max
            }
        );
        assert_eq!(parsed.live.session_idle_seconds, 120);
        assert_eq!(parsed.live.credential_file, credential);
    }
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 2);
}

#[test]
fn voice_invalid_config_is_safe_and_identical_in_both_loaders() {
    let directory = Directory::new().unwrap();
    for (fields, expected) in [
        (
            "session_listen_seconds=2",
            "voice.session_listen_seconds: must be within 3..=300",
        ),
        (
            "session_max_seconds=14",
            "voice.session_max_seconds: must be at least voice.session_listen_seconds",
        ),
        (
            "session_max_seconds='DISCARDED_VALUE'",
            "voice.session_max_seconds: invalid type or value",
        ),
        ("DISCARDED_VALUE=3", "voice: unknown field"),
    ] {
        let input = format!("{MINIMAL}\n[voice]\n{fields}\n");
        for result in [
            BridgeConfig::from_toml_str(&input),
            BridgeConfig::load(directory.write(input).unwrap()),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.to_string(), expected);
            assert!(!format!("{error:?}").contains("DISCARDED_VALUE"));
            assert!(error.source().is_none());
        }
    }
}

#[test]
fn wake_earcon_rejects_types_without_echoing_values() {
    let directory = Directory::new().unwrap();
    for value in ["'DISCARDED_VALUE'", "1", "[]", "{}"] {
        let input = format!("wake_earcon={value}\n{MINIMAL}");
        for result in [
            BridgeConfig::from_toml_str(&input),
            BridgeConfig::load(directory.write(&input).unwrap()),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.to_string(), "wake_earcon: invalid type or value");
            assert!(!format!("{error:?}").contains("DISCARDED_VALUE"));
            assert!(error.source().is_none());
        }
    }
}

#[test]
fn codex_defaults_and_override_precedence_are_offline() {
    let directory = Directory::new().unwrap();
    let home = directory.0.join("uncreated-home");
    let binary = directory.0.join("missing-binary");
    let original = BridgeConfig::from_toml_str(MINIMAL).unwrap();
    assert_eq!(original.codex, echo_config::CodexConfig::default());
    assert_eq!(original.codex_voice(), None);
    assert_eq!(original.codex_model(), None);
    let input = MINIMAL
        .replace("[live]", "[live]\nvoice='cedar'")
        .replace("[agent]", "[agent]\nmodel='legacy-model'");
    let legacy = BridgeConfig::from_toml_str(&input).unwrap();
    assert_eq!(legacy.codex_voice(), Some("cedar"));
    assert_eq!(legacy.codex_model(), Some("legacy-model"));
    let input = format!(
        "{input}\n[codex]\nhome='{}'\nbinary='{}'\nvoice='marin'\nmodel='thread-model'\ninstructions='Local instructions'\nlive_instructions='Local prompt'\n",
        home.display(),
        binary.display()
    );
    let path = directory.write(&input).unwrap();
    let config = BridgeConfig::load(&path).unwrap();
    assert_eq!(config, BridgeConfig::from_toml_str(&input).unwrap());
    assert_eq!(config.codex.home, Some(home.clone()));
    assert_eq!(config.codex.binary, Some(binary.clone()));
    assert_eq!(config.codex_voice(), Some("marin"));
    assert_eq!(config.codex_model(), Some("thread-model"));
    let texts = (
        config.codex.instructions.as_str(),
        config.codex.live_instructions.as_str(),
    );
    assert_eq!(texts, ("Local instructions", "Local prompt"));
    assert!(!home.exists());
    assert!(!binary.exists());
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
}

fn voices(live: Option<&str>, codex: Option<&str>) -> String {
    let mut input = MINIMAL.to_owned();
    if let Some(voice) = live {
        input = input.replace(
            "[live]",
            &format!(
                "[live]
voice='{voice}'"
            ),
        );
    }
    if let Some(voice) = codex {
        input = format!(
            "{input}
[codex]
voice='{voice}'
"
        );
    }
    input
}

#[test]
fn default_voice_is_omitted_and_unknown_voices_are_refused_offline() {
    let example = BridgeConfig::from_toml_str(EXAMPLE).unwrap();
    assert_eq!(example.live.voice.as_deref(), Some("default"));
    assert_eq!(example.codex_voice(), None);
    let load = |live, codex| BridgeConfig::from_toml_str(&voices(live, codex));
    assert_eq!(load(Some("alloy"), Some("default")).unwrap().codex_voice(), None);
    assert_eq!(load(Some("alloy"), Some("cove")).unwrap().codex_voice(), Some("cove"));
    for (live, codex, field) in [
        (Some("Default"), None, "live.voice"),
        (Some("synthetic-voice"), None, "live.voice"),
        (Some("alloy"), Some("alloy "), "codex.voice"),
        (None, Some("synthetic-voice"), "codex.voice"),
    ] {
        assert_eq!(
            load(live, codex).unwrap_err().to_string(),
            format!("{field}: expected default or a pinned realtime voice")
        );
    }
}

#[test]
fn codex_rejects_bad_paths_blank_fields_types_and_inline_secrets_safely() {
    for field in ["home", "binary"] {
        for value in ["''", "'relative/path'", "'~/path'", "false"] {
            let input = format!("{MINIMAL}\n[codex]\n{field}={value}");
            let error = BridgeConfig::from_toml_str(&input).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "codex.{field}: {}",
                    if value == "false" {
                        "invalid type or value"
                    } else {
                        "expected nonempty absolute path"
                    }
                )
            );
        }
    }
    for field in ["voice", "model", "instructions", "live_instructions"] {
        for value in ["''", "'   '", "false"] {
            let input = format!("{MINIMAL}\n[codex]\n{field}={value}");
            let error = BridgeConfig::from_toml_str(&input).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "codex.{field}: {}",
                    if value == "false" {
                        "invalid type or value"
                    } else {
                        "must be nonblank"
                    }
                )
            );
        }
    }
    for (setting, expected) in [
        (
            "api_key='DISCARDED_VALUE'",
            "codex.api_key: secrets go in files, not config",
        ),
        ("DISCARDED_VALUE='private'", "codex: unknown field"),
    ] {
        let error = BridgeConfig::from_toml_str(&format!("{MINIMAL}\n[codex]\n{setting}")).unwrap_err();
        assert_eq!(error.to_string(), expected);
        assert!(!format!("{error:?}").contains("DISCARDED_VALUE"));
    }
}

#[test]
fn minimal_config_defaults_match_both_loaders() {
    let directory = Directory::new().unwrap();
    let config = BridgeConfig::load(directory.write(MINIMAL).unwrap()).unwrap();
    assert_eq!(config, BridgeConfig::from_toml_str(MINIMAL).unwrap());
    assert!(config.wake_earcon);
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
fn muse_loaders_preserve_literal_paths_and_never_create_credential_files() {
    let directory = Directory::new().unwrap();
    let input = format!(
        "{}\n[muse]\nurl='https://127.0.0.1:7448/'\nca_file='absent/ca.der'\ncredential_file='~/absent/muse'\n",
        MINIMAL.replace("[agent]", "[agent]\nbackend='muse'")
    );
    let path = directory.write(&input).unwrap();
    let expected = BridgeConfig::from_toml_str(&input).unwrap();
    for _ in 0..2 {
        assert_eq!(BridgeConfig::load(&path).unwrap(), expected);
    }
    let muse = expected.muse.unwrap();
    assert_eq!(muse.ca_file, PathBuf::from("absent/ca.der"));
    assert_eq!(muse.credential_file, PathBuf::from("~/absent/muse"));
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    assert!(!directory.0.join("absent").exists());
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

mod config_profile;
