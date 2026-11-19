use super::super::{ServeNotice, ServeOptions, SessionNotice};
use super::*;
use echo_link::fake::Command;
use echo_media::MediaPeer;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[path = "voice_context_tests.rs"]
mod voice_context_tests;

// T1: argv admission and the configured owner retain the same original base,
// accepted overlay and safe warning without reopening a changed config file.
#[test]
fn settings_cli_handoff_retains_single_startup_read() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let path = directory.path().join("bridge.toml");
    let text = format!(
        "{}\n[home]\ndir='{}'\n[codex]\nhome='{}'\n",
        include_str!("../../../../../../examples/bridge.example.toml").replace("echo.example.invalid", "127.0.0.1"),
        home.display(),
        directory.path().join("codex").display()
    );
    for (overlay, cap, warning) in [
        ("[agent]\nmax_tool_calls=3\n", 3, false),
        ("[agent]\nmax_tool_calls=0\n", 8, true),
    ] {
        std::fs::write(&path, &text).unwrap();
        std::fs::write(home.join("settings.toml"), overlay).unwrap();
        let args = vec![
            "--config".into(),
            path.to_str().unwrap().into(),
            "--trace".into(),
            directory.path().join("trace").to_str().unwrap().into(),
        ];
        let settings = crate::bridge_input::parse(&args, [None, None, None]).unwrap().unwrap();
        // Any accidental second config/overlay read must fail or see 1.
        std::fs::write(&path, "invalid").unwrap();
        std::fs::write(home.join("settings.toml"), "[agent]\nmax_tool_calls=1\n").unwrap();
        let prepared = super::super::voice_entry::prepare(settings).unwrap();
        assert_eq!(prepared.config.agent.max_tool_calls, cap);
        assert_eq!(prepared.settings_warning.is_some(), warning);
        let startup = prepared.startup.as_ref().unwrap();
        assert_eq!(startup.base.agent.max_tool_calls, 8);
        assert_eq!(startup.config.agent.max_tool_calls, cap);
        assert_eq!(startup.warning.is_some(), warning);
        assert_eq!(
            startup.overlay.apply_to(&startup.base).unwrap().agent.max_tool_calls,
            cap
        );
        assert!(!directory.path().join("trace").exists());
        assert!(!directory.path().join("codex").exists());
    }
}

#[test]
fn configured_capture_watchdog_covers_wait_work_and_playback_with_saturation() {
    for (maximum, response, played, expected) in [
        (600, 30, 5, 605),
        (60, 90, 5, 95),
        (3, 3, 5, 8),
        (2_147_483_647, 30, 5, 2_147_483_647),
        (600, u64::MAX, 5, 2_147_483_647),
        (600, 30, u64::MAX, 2_147_483_647),
    ] {
        let mut prepared = configured();
        prepared.config.voice.session_max_seconds = maximum;
        prepared.config.agent.task_deadline_seconds = response;
        let mut options = prepared.options(None, vec![]);
        options.voice.session.played_timeout = Duration::from_secs(played);
        assert_eq!(options.capture_failsafe_seconds(), expected);
        // Between-wakes reuse and controller listen are independent timers.
        prepared.config.live.session_idle_seconds = 300;
        prepared.config.voice.session_listen_seconds = 3;
        let mut options = prepared.options(None, vec![]);
        options.voice.session.played_timeout = Duration::from_secs(played);
        assert_eq!(options.capture_failsafe_seconds(), expected);
        options.delegation = None;
        assert_eq!(options.capture_failsafe_seconds(), 30);
    }
}

#[tokio::test(start_paused = true)]
async fn configured_call_options_include_context_and_preserve_policy() {
    let prepared = configured().with_mcp_epoch(Some(123));
    assert_eq!(echo_config::RUNNER_INSTRUCTIONS, "Client audio bridge");
    let tool = DynamicTool {
        name: "test.echo".into(),
        description: "Echo".into(),
        input_schema: serde_json::json!({"type":"object"}),
    };
    let options = prepared.options(None, vec![tool.clone()]);
    let text = call_instructions(
        options.voice.call.instructions,
        std::time::UNIX_EPOCH,
        options.voice.call.time_zone,
    )
    .unwrap();
    let call = options
        .voice
        .conversation((&text, "live snapshot"), options.host.deadline);
    assert_eq!(call.call.prompt, "live snapshot");
    assert!(options.voice.call.prompt.contains("Conversation is the default"));
    assert_eq!(options.voice.call.prompt, echo_config::LIVE_INSTRUCTIONS);
    assert_eq!(
        call.call.instructions,
        format!(
            "{}\n[Call-start context: 1970-01-01 Thursday 00:00:00 UTC+00:00; local offset unavailable]", // lint-rules: allow-literal UTC calendar expectation, never an endpoint
            echo_config::RUNNER_INSTRUCTIONS
        )
    );
    assert_eq!(call.call.voice, options.voice.call.voice);
    assert_eq!(call.call.model, options.voice.call.model);
    assert_eq!(call.tools, [tool]);
    assert_eq!(call.session.idle, options.voice.session.idle);
    assert_eq!(call.session.listen_timeout, Some(Duration::from_secs(15)));
    assert_eq!(call.session.session_max, Duration::from_secs(600));
    assert_eq!(call.session.response_timeout, options.voice.session.response_timeout);
    assert_eq!(call.session.played_timeout, options.voice.session.played_timeout);
    assert_eq!(call.session.reply_lead_ms, options.voice.session.reply_lead_ms);
    assert_eq!(call.deadline, options.host.deadline);
    assert_eq!(options.voice.kill_path, Some(prepared.kill_path.as_path()));
    assert_eq!(options.mcp_epoch, Some(123));
    assert_eq!(options.voice.call.instructions, echo_config::RUNNER_INSTRUCTIONS);
}

pub(in crate::bridge_voice) struct MemoryPeer;
impl MediaPeer for MemoryPeer {
    fn create(_: echo_media::PeerConfig) -> impl Future<Output = Result<Self, echo_media::Error>> {
        std::future::ready(Ok(Self))
    }
    fn offer(&self) -> impl Future<Output = Result<String, echo_media::Error>> {
        std::future::ready(Ok("fixture-offer".into()))
    }
    fn apply_answer(&self, _: &str) -> impl Future<Output = Result<(), echo_media::Error>> {
        std::future::ready(Ok(()))
    }
    fn states(&self) -> tokio::sync::watch::Receiver<echo_media::PeerState> {
        tokio::sync::watch::channel(echo_media::PeerState::Connected).1
    }
    fn observe_setup(&self, observer: echo_media::SetupObserver) -> Result<(), echo_media::Error> {
        observer(echo_media::SetupStage::IceConnected)?;
        observer(echo_media::SetupStage::DataChannelOpen)
    }
    fn send(&self, _: echo_media::PcmFrame) -> Result<(), echo_media::Error> {
        Ok(())
    }
    async fn recv(&mut self) -> Result<Option<echo_media::PcmFrame>, echo_media::Error> {
        std::future::pending().await
    }
    fn dropped_frames(&self) -> echo_media::DropCounts {
        echo_media::DropCounts::default()
    }
    fn close(&mut self) -> impl Future<Output = Result<(), echo_media::Error>> {
        std::future::ready(Ok(()))
    }
}
pub(in crate::bridge_voice) fn admission_options() -> ServeOptions<'static> {
    ServeOptions {
        kill_path: None,
        call: bridge_codex::Options {
            voice: Some("cedar"),
            model: Some("fixture-model"),
            effort: Some("low"),
            time_zone: Some("Europe/Berlin"),
            instructions: "configured\n\u{8a9e} \n",
            prompt: "live\n\u{8a9e} \n",
        },
        tools: vec![],
        session: SessionOptions {
            listen_timeout: None,
            session_max: Duration::from_secs(600),
            idle: Duration::from_secs(1),
            response_timeout: Duration::from_secs(1),
            played_timeout: Duration::from_secs(1),
            reply_lead_ms: 200,
            muted: false,
        },
        connect_timeout: Duration::from_secs(2),
    }
}
pub(in crate::bridge_voice) async fn check_context(
    remote: (
        tokio::io::DuplexStream,
        &echo_link::fake::Observer,
        &tokio::sync::Notify,
    ),
    expected: (String, String),
) {
    let (server, observer, started) = remote;
    let mut server = BufReader::new(server);
    for method in ["thread/start", "thread/realtime/start", "thread/realtime/stop"] {
        let mut line = String::new();
        server.read_line(&mut line).await.unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], method);
        let params = &request["params"];
        match method {
            "thread/start" => check_thread_context(params, &expected.0),
            "thread/realtime/start" => {
                check_realtime_context(params, &expected.1);
                server.get_mut().write_all(b"{\"method\":\"thread/realtime/sdp\",\"params\":{\"threadId\":\"thread-1\",\"sdp\":\"fixture-answer\"}}\n").await.unwrap();
            }
            _ => {
                server
                    .get_mut()
                    .write_all(b"{\"method\":\"thread/realtime/closed\",\"params\":{\"threadId\":\"thread-1\"}}\n")
                    .await
                    .unwrap();
            }
        }
        let response = serde_json::json!({"id": request["id"], "result": if method == "thread/start" { serde_json::json!({"thread":{"id":"thread-1"}}) } else { serde_json::json!({}) }});
        server
            .get_mut()
            .write_all(format!("{response}\n").as_bytes())
            .await
            .unwrap();
        if method == "thread/realtime/start" {
            retained_replies(&mut server, observer, started).await;
        }
    }
}
fn check_thread_context(params: &serde_json::Value, expected: &str) {
    assert_eq!(params["baseInstructions"], expected);
    assert_eq!(params["model"], "fixture-model");
    assert_eq!(params["config"]["model_reasoning_effort"], "low");
    assert_eq!(params.get("time_zone"), None);
}
fn check_realtime_context(params: &serde_json::Value, expected: &str) {
    assert_eq!(params["prompt"], expected);
    assert_eq!(params["voice"], "cedar");
    for field in ["model", "model_reasoning_effort", "time_zone"] {
        assert_eq!(params.get(field), None);
    }
}
pub(in crate::bridge_voice) fn retained_observer<'a>(
    observer: &'a echo_link::fake::Observer,
    started: &'a tokio::sync::Notify,
    completed: &'a mut usize,
) -> impl FnMut(ServeNotice) -> Result<(), Error> + 'a {
    move |notice| {
        match notice {
            ServeNotice::Session(SessionNotice::Started) => started.notify_one(),
            ServeNotice::Session(SessionNotice::Completed(_)) => {
                *completed += 1;
                if *completed == 1 {
                    observer.wake("retained", vec![]);
                }
            }
            _ => (),
        }
        Ok(())
    }
}
async fn retained_replies(
    server: &mut BufReader<tokio::io::DuplexStream>,
    observer: &echo_link::fake::Observer,
    started: &tokio::sync::Notify,
) {
    for count in 1..=2 {
        started.notified().await;
        let done = serde_json::json!({"method":"thread/realtime/transcript/done",
            "params":{"threadId":"thread-1","role":"assistant","text":"fixture reply"}});
        server
            .get_mut()
            .write_all(format!("{done}\n").as_bytes())
            .await
            .unwrap();
        let stops = || observer.commands().iter().filter(|cmd| **cmd == Command::Stop).count();
        for _ in 0..1024 {
            if stops() == count {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(stops(), count);
        observer.finish(Ok(()));
    }
}
fn configured() -> Configured {
    let mut config =
        BridgeConfig::from_toml_str(include_str!("../../../../../../examples/bridge.example.toml")).unwrap();
    config.codex.home = Some(std::env::temp_dir().join("unused-codex-config-home"));
    Configured::new(config, "unused-trace".into(), ([127, 0, 0, 1], 0).into()).unwrap()
}

// T1: pending values and active policy are separate owners after exec failure.
#[test]
fn settings_replacement_failure_preserves_active_policy_and_pending_snapshot() {
    use crate::bridge_settings::SettingsHost;
    use echo_screen_contract::settings_v1::{LinkStatusV1, PromptTextsV1};
    use echo_screen_server::{SettingsOperation, SettingsReply};
    let directory = tempfile::tempdir().unwrap();
    let mut prepared = configured();
    prepared.config.home = Some(echo_config::HomeConfig {
        dir: directory.path().join("home"),
        agent_tools: echo_config::AgentTools::default(),
    });
    prepared.trace = directory.path().join("trace");
    let overlay = echo_config::SettingsOverlay::from_toml_str("[agent]\nmax_tool_calls=3\n").unwrap();
    let mut host = SettingsHost::open(
        prepared.config.clone(),
        overlay,
        PromptTextsV1 {
            voice: "voice".into(),
            runner: "runner".into(),
        },
        vec![],
    )
    .unwrap();
    prepared.replacement_failed(&mut host).unwrap();
    let active = prepared.options(None, vec![]);
    assert_eq!(active.delegation.unwrap().agent.max_tool_calls, 8);
    assert_eq!(active.voice.session.listen_timeout, Some(Duration::from_secs(15)));
    assert_eq!(
        active.settings_warning,
        Some("Could not restart bridge; saved settings remain pending")
    );
    let SettingsReply::Snapshot(snapshot) = host.transaction(SettingsOperation::Snapshot, LinkStatusV1::Unconfigured)
    else {
        unreachable!("retained owner refuses snapshot")
    };
    assert_eq!(snapshot.settings.agent.max_tool_calls, 3);
    assert_eq!(snapshot.prompts.voice, "voice");
    assert_eq!(
        snapshot.warnings,
        ["Could not restart bridge; saved settings remain pending"]
    );
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(&prepared.trace)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["event"], "settings.reexec_failed");
    assert_eq!(rows[0]["reason"], "exec");
    assert_eq!(rows[0].as_object().unwrap().len(), 4);
    assert_eq!(std::fs::read_dir(directory.path().join("home")).unwrap().count(), 0);
}

#[test]
fn preparation_and_chatgpt_never_open_legacy_credentials_or_home() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = configured().config;
    config.codex.home = None;
    assert!(matches!(
        Configured::new(config.clone(), "unused".into(), ([127, 0, 0, 1], 0).into()),
        Err(Error::CodexHomeRequired)
    ));
    config.codex.home = Some(directory.path().join("uncreated-home"));
    config.live.mode = LiveMode::Chatgpt;
    config.live.credential_file = directory.path().join("unreadable-credential");
    std::fs::create_dir(&config.live.credential_file).unwrap();
    let prepared = Configured::new(
        config,
        directory.path().join("uncreated-trace"),
        ([127, 0, 0, 1], 0).into(),
    )
    .unwrap();
    assert!(prepared.api_key().unwrap().is_none());
    assert!(!directory.path().join("uncreated-home").exists());
    assert!(!directory.path().join("uncreated-trace").exists());
}
