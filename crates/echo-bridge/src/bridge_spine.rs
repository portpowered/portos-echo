use super::*;
use bridge_audio::{Microphone, Reply, format};
use bridge_health::{Links, response};
use bridge_input::{Provider, parse};
use echo_live::call::{CallError, CredentialProvider, LiveCredential};
use serde_json::json;

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
    };
    let reply = response(b"GET /healthz HTTP/1.1\r\n\r\n", state);
    let body = reply.split_once("\r\n\r\n").unwrap().1;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        json!({"status":"running","wyoming":"connected","live":"idle"})
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
    trace.record("mic.frames", Some("run-1"), json!({"count":32})).unwrap();
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
        assert_configured_provider(settings, mode);
    }
}

fn assert_configured_provider(settings: bridge_input::Settings, mode: &str) {
    if mode == "chatgpt" {
        assert_eq!(settings.mode, echo_live::call::LiveAuthMode::Chatgpt);
        assert_eq!(settings.live_url.split_once("://"), Some(("https", "chatgpt.com")));
        assert!(matches!(settings.credential, Provider::Unavailable));
    } else {
        assert_eq!(settings.mode, echo_live::call::LiveAuthMode::ApiKey);
        assert_eq!(settings.live_url.split_once("://"), Some(("https", "api.openai.com")));
        match settings.credential {
            Provider::ApiKey(key) => assert_eq!(key, std::path::Path::new("credentials/live.json")),
            _ => unreachable!(),
        }
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
