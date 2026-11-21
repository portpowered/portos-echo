use super::EXAMPLE;
use echo_config::{
    AgentConfig, AuditConfig, BridgeConfig, EchoConfig, LiveConfig, LiveMode, McpAuth, McpConfig, McpServer,
    ScreenConfig,
};
use std::path::PathBuf;

#[test]
fn committed_example_loads_with_all_expected_values() {
    let expected = BridgeConfig {
        home: None,
        wake_earcon: true,
        codex: echo_config::CodexConfig::default(),
        muse: None,
        chatgpt_auth: echo_config::ChatGptAuthConfig::default(),
        audio: echo_config::AudioConfig::default(),
        clock: echo_config::ClockConfig::default(),
        voice: echo_config::VoiceConfig::default(),
        trace: echo_config::TraceConfig::default(),
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
            packs: Vec::new(),
            mouth_delay_ms: 40,
        },
        audit: AuditConfig {
            log_dir: "logs/audit".into(),
        },
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/bridge.example.toml");
    assert_eq!(BridgeConfig::load(path).unwrap(), expected);
    assert_eq!(BridgeConfig::from_toml_str(EXAMPLE).unwrap(), expected);
}
