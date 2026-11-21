use super::*;
use bridge_audio::{Microphone, Reply, format};
use bridge_health::{Links, response};
use bridge_input::{Provider, parse};
use echo_live::call::{CallError, CredentialProvider, LiveCredential};
use serde_json::json;

#[test]
fn muse_configured_selects_codex() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bridge.toml");
    let config = include_str!("../../../examples/bridge.example.toml").replace("echo.example.invalid", "127.0.0.1");
    std::fs::write(
        &path,
        format!(
            "{config}\n[muse]\nurl='https://127.0.0.1:7448'\nca_file='absent/ca.der'\ncredential_file='absent/muse'\n"
        ),
    )
    .unwrap();
    let args = vec![
        "--config".into(),
        path.to_string_lossy().into_owned(),
        "--trace".into(),
        "unused-trace".into(),
    ];
    let settings = parse(&args, [None, None, None]).unwrap().unwrap();
    assert!(settings.config.as_ref().unwrap().muse.is_some());
    assert!(bridge_runtime::configured(&settings));
}

#[test]
fn bridge_lifecycle_lab_idle_is_explicit_validated_and_never_changes_configured_mode() {
    let mut settings = parse(
        &[],
        [
            Some("127.0.0.1:1".into()),
            Some("http://127.0.0.1:2".into()),
            Some("trace".into()),
        ],
    )
    .unwrap()
    .unwrap();
    settings.lab_idle(None).unwrap();
    assert_eq!(settings.session_idle_seconds, 120);
    for value in ["0", "1", "18446744073709551615"] {
        settings.lab_idle(Some(value)).unwrap();
        assert_eq!(settings.session_idle_seconds, value.parse::<u64>().unwrap());
    }
    for value in ["", "-1", "+1", "1.0", " 1", "18446744073709551616"] {
        assert_eq!(settings.lab_idle(Some(value)), Err(Error::Input));
        assert_eq!(settings.session_idle_seconds, u64::MAX);
    }
    settings.lab = false;
    assert_eq!(settings.lab_idle(Some("1")), Err(Error::Input));
    settings.lab_idle(None).unwrap();
    assert_eq!(settings.session_idle_seconds, u64::MAX);
}

#[test]
fn bridge_spine_ten_wire_chunks_are_32_packets_and_exact_silence() {
    let mut mic = Microphone::new(format(16_000)).unwrap();
    let mut packets = Vec::new();
    for _ in 0..10 {
        packets.extend(mic.process(&format(16_000), &[0; 2048]).unwrap());
    }
    packets.extend(mic.finish().unwrap());
    assert_eq!((mic.input_samples, mic.packets, mic.tail_samples()), (10240, 32, 0));
    assert_eq!(packets.len(), 32);
    let mut reply = Reply::new().unwrap();
    let mut pcm = Vec::new();
    for (index, packet) in packets.iter().enumerate() {
        assert_eq!(usize::from(packet.sequence), index);
        assert_eq!(packet.timestamp, u32::try_from(index).unwrap() * 960);
        pcm.extend(reply.process(packet).unwrap());
    }
    pcm.extend(reply.finish().unwrap());
    assert_eq!(pcm, vec![0; 32 * 960]);
}

#[test]
fn bridge_spine_partial_tail_and_malformed_audio_are_explicit() {
    let mut mic = Microphone::new(format(16_000)).unwrap();
    let mut packets = mic.process(&format(16_000), &[0; 202]).unwrap();
    packets.extend(mic.finish().unwrap());
    assert_eq!((packets.len(), mic.input_samples, mic.tail_samples()), (0, 101, 303));
    assert_eq!(Microphone::new(format(44100)).err(), Some(Error::Audio));
    let mut mic = Microphone::new(format(16_000)).unwrap();
    assert_eq!(mic.process(&format(24_000), &[0; 2]), Err(Error::Audio));
    assert_eq!(mic.process(&format(16_000), &[0]), Err(Error::Audio));
    let mut reply = Reply::new().unwrap();
    assert_eq!(reply.finish().unwrap(), Vec::<u8>::new());
    assert_eq!(
        reply.process(&echo_live::AudioFrame {
            sequence: 0,
            timestamp: 0,
            payload: vec![]
        }),
        Err(Error::Audio)
    );
}

#[test]
fn bridge_spine_health_has_exact_states_methods_and_empty_errors() {
    let state = Links {
        wyoming: "connected",
        live: "idle",
        ..Links::default()
    };
    let reply = response(b"GET /healthz HTTP/1.1\r\n\r\n", state);
    let body = reply.split_once("\r\n\r\n").unwrap().1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        json!({"status":"running","wyoming":"connected","live":"idle","relink_required":false,"kill_switch":false,
            "mcp":"disabled"})
    );
    assert_eq!(
        response(b"POST /healthz HTTP/1.1\r\n\r\n", state),
        "HTTP/1.1 405 Method Not Allowed\r\nAllow: GET\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    assert!(response(b"GET /missing HTTP/1.1\r\n\r\n", state).starts_with("HTTP/1.1 404 Not Found\r\n"));
    assert_eq!(response(b"broken", state), "");
}

#[test]
fn bridge_spine_startup_rejects_partial_mixed_and_nonloopback_inputs_offline() {
    let lab = [
        Some("127.0.0.1:16700".into()),
        Some("http://127.0.0.1:18081".into()),
        Some("unused".into()),
    ];
    let settings = parse(&[], lab.clone()).unwrap().unwrap();
    assert_eq!(settings.wyoming.port(), 16700);
    assert_eq!(settings.health.port(), 0);
    assert!(settings.lab);
    for args in [
        vec!["--config".into(), "unused".into()],
        vec!["--bad".into()],
        vec!["--health-listen".into(), "localhost:1".into()],
    ] {
        assert_eq!(parse(&args, lab.clone()).err(), Some(Error::Input));
    }
    assert_eq!(parse(&[], [lab[0].clone(), None, None]).err(), Some(Error::Input));
    for address in ["localhost:1", "127.0.0.1:0", "bad"] {
        assert_eq!(bridge_input::loopback(address, false), Err(Error::Input));
    }
    assert_eq!(bridge_input::lab_origin("http://127.0.0.1:1/path"), Err(Error::Input));
}

#[test]
fn bridge_spine_configured_echo_is_any_ip_literal_on_the_lan() {
    // TEST-NET-1 and documentation IPv6: the PC bridge dials the Echo across the LAN, not only loopback.
    for (host, want) in [
        ("192.0.2.10", "192.0.2.10:16700"), // lint-rules: allow-literal inert documentation address, parsed only, never dialed
        ("2001:db8::7", "[2001:db8::7]:16700"), // lint-rules: allow-literal inert documentation address, parsed only, never dialed
        ("[2001:db8::7]", "[2001:db8::7]:16700"), // lint-rules: allow-literal inert documentation address, parsed only, never dialed
        ("127.0.0.1", "127.0.0.1:16700"),
    ] {
        let address = bridge_input::echo_address(host, 16700).unwrap();
        assert_eq!(address.to_string(), want);
    }
    assert_eq!(bridge_input::echo_address("192.0.2.10", 0), Err(Error::Input)); // lint-rules: allow-literal inert documentation address, parsed only, never dialed
    assert_eq!(bridge_input::echo_address("bad host", 16700), Err(Error::Input));
}

#[tokio::test]
async fn bridge_spine_credentials_read_only_nominated_synthetic_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic");
    assert!(matches!(
        Provider::ApiKey(path.clone()).credential().await,
        Err(CallError::Authorization)
    ));
    assert!(matches!(
        Provider::Unavailable.credential().await,
        Err(CallError::Authorization)
    ));
    for invalid in ["", "\n", "bad\ninput", "bad\r"] {
        std::fs::write(&path, invalid).unwrap();
        assert!(matches!(
            Provider::ApiKey(path.clone()).credential().await,
            Err(CallError::Authorization)
        ));
    }
    std::fs::write(&path, "synthetic-local-only\r\n").unwrap();
    match Provider::ApiKey(path).credential().await.unwrap() {
        LiveCredential::ApiKey { key } => assert_eq!(key.expose(), "synthetic-local-only"),
        LiveCredential::Chatgpt { .. } => unreachable!(),
    }
}

#[test]
fn bridge_spine_trace_preserves_aliases_counters_and_safe_mode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace");
    let mut trace = bridge_trace::Trace::open(&path, "chatgpt").unwrap();
    trace
        .record("wyoming.mic_frames", Some("run-1"), json!({"count":10}))
        .unwrap();
    trace
        .record("mic.completed", Some("run-1"), json!({"count":32}))
        .unwrap();
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(
        lines.iter().map(|v| v["count"].as_u64().unwrap()).collect::<Vec<_>>(),
        [10, 32]
    );
    for line in lines {
        assert_eq!(line["provider_auth_mode"], "chatgpt");
        assert_eq!(line["run_id"], "run-1");
    }
    assert_eq!(
        bridge_trace::Trace::open(dir.path(), "chatgpt").err(),
        Some(Error::Trace)
    );
}

#[test]
fn bridge_spine_configured_modes_preserve_paths_and_never_read_credentials_at_parse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bridge.toml");
    let template = include_str!("../../../examples/bridge.example.toml").replace("echo.example.invalid", "127.0.0.1");
    let args = vec![
        "--config".into(),
        path.to_string_lossy().into_owned(),
        "--trace".into(),
        "trace.jsonl".into(),
    ];
    for mode in ["chatgpt", "api_key"] {
        std::fs::write(
            &path,
            template.replace("mode = \"chatgpt\"", &format!("mode = \"{mode}\"")),
        )
        .unwrap();
        let settings = parse(&args, [None, None, None]).unwrap().unwrap();
        assert_eq!(settings.wyoming.to_string(), "127.0.0.1:16700");
        assert_eq!(settings.health.to_string(), "127.0.0.1:16901");
        assert_eq!(settings.trace, std::path::PathBuf::from("trace.jsonl"));
        assert_eq!(settings.voice, "default");
        assert_eq!(settings.session_idle_seconds, 120);
        assert!(!settings.lab);
        assert!(!settings.sim_clock);
        assert_eq!(settings.sim_epoch, None);
        assert_configured_provider(&settings, mode);
    }
}

fn assert_configured_provider(settings: &bridge_input::Settings, mode: &str) {
    assert!(matches!(settings.credential, Provider::Unavailable));
    let config = settings.config.as_ref().unwrap();
    assert_eq!(
        config.live.credential_file,
        std::path::PathBuf::from("credentials/live.json")
    );
    assert_eq!(config.codex.home, None);
    if mode == "chatgpt" {
        assert_eq!(settings.mode, echo_live::call::LiveAuthMode::Chatgpt);
        assert_eq!(settings.live_url.split_once("://"), Some(("https", "chatgpt.com")));
        assert_eq!(config.live.mode, echo_config::LiveMode::Chatgpt);
    } else {
        assert_eq!(settings.mode, echo_live::call::LiveAuthMode::ApiKey);
        assert_eq!(settings.live_url.split_once("://"), Some(("https", "api.openai.com")));
        assert_eq!(config.live.mode, echo_config::LiveMode::ApiKey);
    }
}

#[test]
fn bridge_spine_config_errors_and_duplicate_flags_are_offline() {
    assert!(parse(&["--help".into()], [None, None, None]).unwrap().is_none());
    for args in [
        vec![],
        vec!["--config", "missing"],
        vec!["--trace"],
        vec!["--trace", "one", "--trace", "two"],
        vec!["--health-listen", "127.0.0.1:0", "--health-listen", "127.0.0.1:0"],
    ] {
        let args: Vec<_> = args.into_iter().map(String::from).collect();
        assert_eq!(parse(&args, [None, None, None]).err(), Some(Error::Input));
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "malformed [").unwrap();
    assert_eq!(
        parse(
            &["--config".into(), path.to_string_lossy().into_owned()],
            [None, None, None]
        )
        .err(),
        Some(Error::Input)
    );
}

#[tokio::test]
async fn bridge_spine_credential_bytes_are_bounded_and_utf8_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic");
    let provider = Provider::ApiKey(path.clone());
    for bytes in [vec![255], vec![b'x'; 16_385], vec![b'x', 0]] {
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(provider.credential().await, Err(CallError::Authorization)));
    }
    std::fs::write(&path, vec![b'x'; 16_384]).unwrap();
    match provider.credential().await.unwrap() {
        LiveCredential::ApiKey { key } => assert_eq!(key.expose(), &"x".repeat(16_384)),
        LiveCredential::Chatgpt { .. } => unreachable!(),
    }
}

#[test]
fn bridge_wire_mock_origins_require_complete_pair_and_local_dial_targets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let template = include_str!("../../../examples/bridge.example.toml")
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("0.0.0.0:16900", "127.0.0.1:0"); // lint-rules: allow-literal inert example replacement
    let local = format!("{template}\n[chatgpt_auth]\ntoken_endpoint = 'http://127.0.0.1:1/token'");
    let args: Vec<String> = vec![
        "--config".into(),
        path.to_string_lossy().into_owned(),
        "--trace".into(),
        "unused".into(),
    ];
    for origin in [
        "http://127.0.0.1:1",
        "http://127.0.0.1:1/",
        "http://localhost:1",
        "http://127.0.0.1:0",
        "http://127.0.0.1:1/path",
        "http://127.0.0.1:1?x",
        "http://user@127.0.0.1:1",
    ] {
        std::fs::write(&path, &local).unwrap();
        let mut paired = args.clone();
        paired.extend([
            "--live-loopback".into(),
            origin.into(),
            "--model-loopback".into(),
            "http://127.0.0.1:2".into(),
        ]);
        if origin == "http://127.0.0.1:1" {
            let settings = parse(&paired, [None, None, None]).unwrap().unwrap();
            assert_eq!(settings.live_url, origin);
            assert_eq!(settings.model_url.as_deref(), Some("http://127.0.0.1:2"));
            for invalid in [
                template.clone(),
                local.replace("127.0.0.1:18080", "localhost:18080"),
                local.replace("127.0.0.1:0", "[::1]:0"),
            ] {
                std::fs::write(&path, invalid).unwrap();
                assert_eq!(parse(&paired, [None, None, None]).err(), Some(Error::Input));
            }
        } else {
            assert_eq!(parse(&paired, [None, None, None]).err(), Some(Error::Input));
        }
        paired.truncate(6);
        assert_eq!(parse(&paired, [None, None, None]).err(), Some(Error::Input));
    }
}

#[test]
fn mcp_link_epoch_requires_guard_and_local_overrides_and_unsigned_value() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let template = include_str!("../../../examples/bridge.example.toml")
        .replace("echo.example.invalid", "127.0.0.1")
        .replace(&format!("{}:16900", std::net::Ipv4Addr::UNSPECIFIED), "127.0.0.1:0");
    std::fs::write(
        &path,
        format!("{template}\n[chatgpt_auth]\ntoken_endpoint = 'http://127.0.0.1:1/token'"),
    )
    .unwrap();
    let args: Vec<String> = [
        "--config",
        path.to_str().unwrap(),
        "--trace",
        "unused",
        "--live-loopback",
        "http://127.0.0.1:1",
        "--model-loopback",
        "http://127.0.0.1:2",
        "--sim-clock",
        "--sim-epoch",
        "1800000000",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let settings = parse(&args, [None, None, None]).unwrap().unwrap();
    assert!(settings.sim_clock);
    assert_eq!(settings.sim_epoch, Some(1_800_000_000));
    for value in ["", "-1", "+1", "1.5", "18446744073709551616"] {
        let mut invalid = args.clone();
        *invalid.last_mut().unwrap() = value.into();
        assert_eq!(parse(&invalid, [None, None, None]).err(), Some(Error::Input));
    }
    let mut no_guard = args.clone();
    no_guard.remove(8);
    assert_eq!(parse(&no_guard, [None, None, None]).err(), Some(Error::Input));
    let mut no_overrides = args.clone();
    no_overrides.drain(4..8);
    assert_eq!(parse(&no_overrides, [None, None, None]).err(), Some(Error::Input));
    assert_eq!(
        parse(&args, [Some("synthetic".into()), None, None]).err(),
        Some(Error::Input)
    );
    let mut duplicate = args;
    duplicate.extend(["--sim-epoch".into(), "42".into()]);
    assert_eq!(parse(&duplicate, [None, None, None]).err(), Some(Error::Input));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn startup_network_waiting_reason_is_optional_in_health_response() {
    let mut links = Links::default();
    for waiting in [None, Some("dependency_unavailable"), None] {
        links.startup_waiting_reason = waiting;
        let response = response(b"GET /healthz HTTP/1.1\r\n\r\n", links);
        let body: serde_json::Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["relink_required"], false);
        assert_eq!(
            body.get("startup_waiting_reason").and_then(serde_json::Value::as_str),
            waiting
        );
    }
}
