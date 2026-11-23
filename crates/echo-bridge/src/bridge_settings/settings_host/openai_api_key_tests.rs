use super::*;
use echo_config::{AgentTools, BridgeConfig, HomeConfig, SettingsOverlay};
use echo_screen_contract::settings_v1::{LinkStatusV1, PromptTextsV1};
use echo_screen_server::{SettingsOperation, SettingsReply};
use std::os::unix::fs::PermissionsExt;

fn host(mode: LiveMode) -> (tempfile::TempDir, SettingsHost) {
    let root = tempfile::tempdir().unwrap();
    let mut base = BridgeConfig::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/bridge.echo.toml")).unwrap();
    base.home = Some(HomeConfig {
        dir: root.path().join("home"),
        agent_tools: AgentTools::default(),
    });
    base.live.mode = mode;
    base.live.credential_file = root.path().join("live.json");
    let prompts = PromptTextsV1 {
        voice: "Synthetic voice".into(),
        runner: "Synthetic runner".into(),
    };
    let host = SettingsHost::open(base, SettingsOverlay::default(), prompts, Vec::new()).unwrap();
    (root, host)
}

fn save(host: &mut SettingsHost, key: &str) -> SettingsReply {
    let request = serde_json::from_value(serde_json::json!({"version":2, "api_key":key})).unwrap();
    host.transaction(SettingsOperation::OpenAiApiKey(request), LinkStatusV1::Unconfigured)
}

fn private(path: &Path) -> (String, u32) {
    let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    (std::fs::read_to_string(path).unwrap(), mode)
}

fn invalid(reply: SettingsReply) -> Option<String> {
    match reply {
        SettingsReply::InvalidV2(error) => Some(error.message),
        _ => None,
    }
}

fn saved(reply: SettingsReply) -> Option<bool> {
    match reply {
        SettingsReply::SavedV2(saved) => Some(saved.restart_pending),
        _ => None,
    }
}

const RULE: &str = "openai.api_key: expected one line of printable text";

#[test]
fn openai_api_key_save_refuses_blank_and_control_text_without_writing() {
    let (root, mut host) = host(LiveMode::ApiKey);
    assert_eq!(host.api_key(), Some(OpenAiApiKeyStatusV2 { configured: false }));
    let oversized = "k".repeat(16_385);
    for text in [
        "",
        "\n",
        "  ",
        "dummy\u{7}local",
        "dummy\nlocal",
        "dummy\u{85}",
        " dummy-local-only",
        "dummy-local-only ",
        oversized.as_str(),
    ] {
        assert_eq!(invalid(save(&mut host, text)).as_deref(), Some(RULE), "{text:?}");
    }
    assert!(!root.path().join("live.json").exists());
    assert_eq!(host.api_key(), Some(OpenAiApiKeyStatusV2 { configured: false }));
}

#[test]
fn openai_api_key_save_replaces_privately_and_keeps_the_previous_key() {
    let (root, mut host) = host(LiveMode::ApiKey);
    let file = root.path().join("live.json");
    assert_eq!(saved(save(&mut host, "dummy-local-only\r\n")), Some(true));
    assert_eq!(private(&file), ("dummy-local-only".into(), 0o600));
    assert!(!root.path().join("live.json.previous").exists());
    assert_eq!(host.api_key(), Some(OpenAiApiKeyStatusV2 { configured: true }));
    assert_eq!(saved(save(&mut host, "dummy-local-two")), Some(true));
    assert_eq!(private(&file), ("dummy-local-two".into(), 0o600));
    assert_eq!(
        private(&root.path().join("live.json.previous")),
        ("dummy-local-only".into(), 0o600)
    );
    let temporary = std::fs::read_dir(root.path())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".screen-save")
        })
        .count();
    assert_eq!(temporary, 0);
    let reply = host.transaction(
        SettingsOperation::SnapshotV2 { include_muse: true },
        LinkStatusV1::Unconfigured,
    );
    let SettingsReply::SnapshotV2(snapshot) = reply else {
        return assert!(matches!(reply, SettingsReply::SnapshotV2(_)));
    };
    assert!(snapshot.restart_pending);
    assert!(!serde_json::to_string(&snapshot).unwrap().contains("dummy-local"));
}

#[test]
fn openai_api_key_save_refuses_chatgpt_mode_and_unavailable_restart() {
    let (root, mut chatgpt) = host(LiveMode::Chatgpt);
    assert_eq!(chatgpt.api_key(), None);
    assert_eq!(
        invalid(save(&mut chatgpt, "dummy-local-only")).as_deref(),
        Some("openai.api_key: this Echo uses ChatGPT sign-in")
    );
    assert!(!root.path().join("live.json").exists());
    let (root, mut fixed) = host(LiveMode::ApiKey);
    fixed.disable_restart();
    assert!(matches!(
        save(&mut fixed, "dummy-local-only"),
        SettingsReply::Unavailable
    ));
    assert!(!root.path().join("live.json").exists());
}
