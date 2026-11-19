use super::*;
use std::io::Write;

#[test]
fn context_fixed_utc_calendar_and_prefix() {
    for (epoch, expected) in [
        (0_i64, "1970-01-01 Thursday 00:00:00"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
        (-1, "1969-12-31 Wednesday 23:59:59"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
        (951_868_799, "2000-02-29 Tuesday 23:59:59"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
        (4_107_542_400, "2100-03-01 Monday 00:00:00"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
        (1_798_761_599, "2026-12-31 Thursday 23:59:59"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
        (1_798_761_600, "2027-01-01 Friday 00:00:00"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
        (1_794_918_896, "2026-11-17 Tuesday 12:34:56"), // lint-rules: allow-literal UTC calendar expectation, never an endpoint
    ] {
        let now = if epoch < 0 {
            std::time::UNIX_EPOCH - Duration::from_secs(epoch.unsigned_abs())
        } else {
            std::time::UNIX_EPOCH + Duration::from_secs(epoch.unsigned_abs())
        };
        for prefix in ["Client audio bridge", "\u{8a9e}\n[Call-start context: authored] \n"] {
            assert_eq!(
                call_instructions(prefix, now, None).unwrap(),
                format!("{prefix}\n[Call-start context: {expected} UTC+00:00; local offset unavailable]") // lint-rules: allow-literal UTC calendar expectation, never an endpoint
            );
        }
    }
    assert_eq!(
        call_instructions("", std::time::UNIX_EPOCH - Duration::from_nanos(100), None).unwrap(),
        "\n[Call-start context: 1969-12-31 Wednesday 23:59:59 UTC+00:00; local offset unavailable]" // lint-rules: allow-literal UTC calendar expectation, never an endpoint
    );
    let invalid = std::time::UNIX_EPOCH + Duration::from_hours(70_389_528);
    assert_eq!(call_instructions("prefix", invalid, None), Err(Error::Input));
}

#[test]
fn configured_zone_preserves_prefix_and_fixed_utc_snapshot() {
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_794_918_896);
    for prefix in ["Client audio bridge", "\u{8a9e}\n[Call-start context: authored] \n"] {
        for (zone, suffix) in [
            (None, "local offset unavailable"),
            (
                Some("Europe/Berlin"),
                "operator time zone: Europe/Berlin; answer time questions in Europe/Berlin",
            ),
        ] {
            let actual = call_instructions(prefix, now, zone).unwrap();
            assert_eq!(
                actual,
                format!("{prefix}\n[Call-start context: 2026-11-17 Tuesday 12:34:56 UTC+00:00; {suffix}]") // lint-rules: allow-literal UTC calendar expectation, never an endpoint
            );
            writeln!(std::io::stdout(), "observed instructions: {actual}").unwrap();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn configured_options_preserve_overrides_budgets_tools_and_default_voice() {
    let mut prepared = configured();
    prepared.config.codex.delegate_model = Some("delegate-model".into());
    prepared.config.codex.delegate_effort = Some("custom-effort".into());
    prepared.config.codex.time_zone = Some("Europe/Berlin".into());
    prepared.config.codex.voice = Some("cedar".into());
    prepared.config.live.voice = Some("alloy".into());
    prepared.config.codex.model = Some("configured-model".into());
    prepared.config.agent.model = Some("legacy-model".into());
    prepared.config.live.session_idle_seconds = 43;
    prepared.config.agent.task_deadline_seconds = 17;
    prepared.config.audio.reply_lead_ms = 120;
    let tool = DynamicTool {
        name: "test.echo".into(),
        description: "Echo".into(),
        input_schema: serde_json::json!({"type":"object"}),
    };
    let options = prepared.options(None, vec![tool.clone()]);
    assert_eq!(options.mcp_epoch, None);
    assert_eq!(options.voice.call.voice, Some("cedar"));
    assert_eq!(options.voice.call.model, Some("delegate-model"));
    let call = options
        .voice
        .conversation(("snapshot", "live snapshot"), options.host.deadline);
    assert_eq!(call.call.effort, Some("custom-effort"));
    assert_eq!(call.call.time_zone, Some("Europe/Berlin"));
    assert_eq!(options.voice.call.instructions, echo_config::RUNNER_INSTRUCTIONS);
    assert_eq!(options.voice.tools, [tool]);
    assert_eq!(options.voice.session.idle, Duration::from_secs(43));
    assert_eq!(options.voice.session.response_timeout, Duration::from_secs(17));
    assert_eq!(options.voice.session.reply_lead_ms, 120);
    assert_eq!(options.voice.kill_path, Some(prepared.kill_path.as_path()));
    assert_eq!(options.host.deadline, Instant::now() + Duration::from_secs(30));
    prepared.config.codex.delegate_model = None;
    assert_eq!(
        prepared.options(None, vec![]).voice.call.model,
        Some("configured-model")
    );
    prepared.config.codex.voice = None;
    prepared.config.codex.model = None;
    assert_eq!(prepared.options(None, vec![]).voice.call.voice, Some("alloy"));
    assert_eq!(prepared.options(None, vec![]).voice.call.model, Some("legacy-model"));
    prepared.config.live.voice = None;
    prepared.config.agent.model = None;
    assert_eq!(prepared.options(None, vec![]).voice.call.voice, None);
    assert_eq!(prepared.options(None, vec![]).voice.call.model, None);
    let prepared = prepared.with_mcp_epoch(Some(1_800_000_000));
    assert_eq!(prepared.options(None, vec![]).mcp_epoch, Some(1_800_000_000));
}

#[test]
fn screen_settings_followup_off_uses_existing_answered_turn_completion_policy() {
    let mut prepared = configured();
    for seconds in [3, 15, 300] {
        prepared.config.voice.session_listen_seconds = seconds;
        prepared.config.voice.session_listen_enabled = true;
        let options = prepared.options(None, vec![]);
        assert_eq!(
            options.voice.session.listen_timeout,
            Some(Duration::from_secs(u64::from(seconds)))
        );
        prepared.config.voice.session_listen_enabled = false;
        let options = prepared.options(None, vec![]);
        assert_eq!(options.voice.session.listen_timeout, None);
        assert_eq!(options.voice.session.session_max, Duration::from_secs(600));
        assert_eq!(options.voice.session.idle, Duration::from_secs(120));
    }
}
