//! T3: production admission/RPC, memory link/media, paused clock and real files.
use super::super::{
    prompts,
    voice_config::tests::{MemoryPeer, admission_options, retained_observer},
};
use super::*;
use crate::bridge_trace::Trace;
use echo_link::{
    LinkConnector,
    fake::{Command, FakeConnector, Observer},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test(start_paused = true)]
async fn t3_agent_home_edits_reload_exact_rpc_and_bad_edits_retain_previous() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("prompts");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("runner.md"), "runner one\n\u{8a9e} \n").unwrap();
    std::fs::write(dir.join("voice.md"), "voice one\n").unwrap();
    std::fs::write(dir.join("edit-during-retained"), "").unwrap();
    let path = root.path().join("trace");
    admissions(Some(root.path()), &path, 4, |index| {
        let (runner, voice) = edit(&dir, index);
        (runner.into(), voice.into())
    })
    .await;
    check_prompt_trace(&path);
}

async fn admissions(
    home: Option<&std::path::Path>,
    path: &std::path::Path,
    steps: usize,
    edit: impl Fn(usize) -> (String, String),
) {
    let trace = Trace::open(path, "api_key").unwrap();
    prompts::scope(home, async {
        let (stream, server) = tokio::io::duplex(65_536);
        let (read, write) = tokio::io::split(stream);
        let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
        let observer = Observer::default();
        let mut client = FakeConnector::new(observer.clone()).connect().await.unwrap();
        let mut events = client.events().unwrap();
        let mut options = admission_options();
        options.call.time_zone = None;
        let mut server = BufReader::new(server);
        for index in 0..steps {
            let expected = edit(index);
            observer.wake("admit", vec![]);
            let LinkEvent::Wake(run) = events.recv().await.unwrap().unwrap() else {
                unreachable!()
            };
            let started = tokio::sync::Notify::new();
            let mut completed = 0;
            let mut observe = retained_observer(&observer, &started, &mut completed);
            let mut peer = async || Ok(MemoryPeer);
            let work = admitted(
                (&client, &mut events, run),
                &mut io,
                &mut options,
                &mut peer,
                std::future::pending(),
                &mut observe,
                (None, || std::time::UNIX_EPOCH, Some(trace.clone())),
            );
            let remote = prompt_remote(
                &mut server,
                &observer,
                &started,
                (&expected.0, &expected.1),
                index,
                home,
            );
            let (result, ()) = tokio::join!(work, remote);
            assert_eq!(result, Ok(()));
            drop(observe);
            assert_eq!(completed, 2);
        }
        Ok(())
    })
    .await
    .unwrap();
}

fn check_prompt_trace(path: &std::path::Path) {
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|row: &serde_json::Value| row["event"].as_str().unwrap().starts_with("prompt."))
        .collect();
    assert_eq!(
        rows.iter()
            .map(|row| (row["event"].as_str().unwrap(), row["prompt"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        [
            ("prompt.reload_refused", "runner"),
            ("prompt.reload_refused", "voice"),
            ("prompt.defaulted", "runner"),
            ("prompt.defaulted", "voice")
        ]
    );
    assert_eq!(rows[0]["reason"], "empty");
    assert_eq!(rows[1]["reason"], "oversized");
}

fn edit(dir: &std::path::Path, index: usize) -> (&'static str, &'static str) {
    match index {
        0 => ("runner one\n\u{8a9e} \n", "voice one\n"),
        1 => {
            std::fs::write(dir.join("runner.md"), "runner two").unwrap();
            std::fs::write(dir.join("voice.md"), "voice two").unwrap();
            ("runner two", "voice two")
        }
        2 => {
            std::fs::write(dir.join("runner.md"), " \n").unwrap();
            std::fs::write(dir.join("voice.md"), vec![b'x'; 32_769]).unwrap();
            ("runner two", "voice two")
        }
        _ => {
            std::fs::remove_file(dir.join("runner.md")).unwrap();
            std::fs::remove_file(dir.join("voice.md")).unwrap();
            (echo_config::RUNNER_INSTRUCTIONS, echo_config::LIVE_INSTRUCTIONS)
        }
    }
}

async fn prompt_remote(
    server: &mut BufReader<tokio::io::DuplexStream>,
    observer: &Observer,
    started: &tokio::sync::Notify,
    expected: (&str, &str),
    index: usize,
    home: Option<&std::path::Path>,
) {
    let suffix = "\n[Call-start context: 1970-01-01 Thursday 00:00:00 UTC+00:00; local offset unavailable]"; // lint-rules: allow-literal UTC calendar expectation, never an endpoint
    let thread = format!("thread-{index}");
    for method in ["thread/start", "thread/realtime/start", "thread/realtime/stop"] {
        let mut line = String::new();
        server.read_line(&mut line).await.unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], method);
        let result = if method == "thread/start" {
            assert_eq!(request["params"]["baseInstructions"], format!("{}{suffix}", expected.0));
            assert_eq!(request["params"]["model"], "fixture-model");
            assert_eq!(request["params"]["config"]["model_reasoning_effort"], "low");
            serde_json::json!({"thread":{"id":thread}})
        } else {
            assert_eq!(request["params"]["threadId"], thread);
            if method == "thread/realtime/start" {
                assert_eq!(request["params"]["prompt"], format!("{}{suffix}", expected.1));
                assert_eq!(request["params"]["voice"], "cedar");
                let event = serde_json::json!({"method":"thread/realtime/sdp","params":{"threadId":thread,"sdp":"fixture-answer"}});
                send(server, &event).await;
            } else {
                let event = serde_json::json!({"method":"thread/realtime/closed","params":{"threadId":thread}});
                send(server, &event).await;
            }
            serde_json::json!({})
        };
        let response = serde_json::json!({"id":request["id"],"result":result});
        send(server, &response).await;
        if method == "thread/realtime/start" {
            if index == 0
                && let Some(home) = home
                && home.join("prompts/edit-during-retained").exists()
            {
                for kind in ["voice", "runner"] {
                    std::fs::write(home.join(format!("prompts/{kind}.md")), "edited during retained call").unwrap();
                }
            }
            replies(server, observer, started, &thread).await;
        }
    }
}

async fn send(server: &mut BufReader<tokio::io::DuplexStream>, value: &serde_json::Value) {
    server
        .get_mut()
        .write_all(format!("{value}\n").as_bytes())
        .await
        .unwrap();
}

async fn replies(
    server: &mut BufReader<tokio::io::DuplexStream>,
    observer: &Observer,
    started: &tokio::sync::Notify,
    thread: &str,
) {
    let stops = || observer.commands().iter().filter(|cmd| **cmd == Command::Stop).count();
    let initial = stops();
    for count in 1..=2 {
        started.notified().await;
        let event = serde_json::json!({"method":"thread/realtime/transcript/done","params":{"threadId":thread,"role":"assistant","text":"fixture reply"}});
        server
            .get_mut()
            .write_all(format!("{event}\n").as_bytes())
            .await
            .unwrap();
        for _ in 0..1024 {
            if stops() == initial + count {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(stops(), initial + count);
        observer.finish(Ok(()));
    }
}

#[tokio::test(start_paused = true)]
async fn t3_agent_home_configured_startup_refuses_before_credentials_child_or_device() {
    configured_refusal("home", b"", "directory unavailable").await;
    for kind in ["runner", "voice"] {
        for (bytes, message) in [
            (vec![], "empty prompt"),
            (vec![b'x'; 32_769], "prompt exceeds 32768 bytes"),
        ] {
            configured_refusal(kind, &bytes, message).await;
        }
    }
}

async fn configured_refusal(kind: &str, bytes: &[u8], message: &str) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    if kind == "home" {
        std::fs::write(&home, "file").unwrap();
    } else {
        std::fs::create_dir_all(home.join("prompts")).unwrap();
        std::fs::write(home.join(format!("prompts/{kind}.md")), bytes).unwrap();
    }
    let mut config =
        echo_config::BridgeConfig::from_toml_str(include_str!("../../../../../../examples/bridge.example.toml"))
            .unwrap();
    config.home = Some(echo_config::HomeConfig { dir: home });
    config.codex.home = Some(root.path().join("codex"));
    config.live.mode = echo_config::LiveMode::ApiKey;
    config.live.credential_file = root.path().join("absent-key");
    let trace = root.path().join("trace");
    let observer = Observer::default();
    let prepared = super::super::Configured::new(config, trace.clone(), ([127, 0, 0, 1], 0).into()).unwrap();
    let result = prepared
        .execute(
            FakeConnector::new(observer.clone()),
            async || -> Result<MemoryPeer, Error> { unreachable!() },
            vec![],
            std::future::pending(),
        )
        .await;
    let field = if kind == "home" {
        "home.dir".into()
    } else {
        format!("prompts/{kind}.md")
    };
    assert_eq!(result.unwrap_err().to_string(), format!("{field}: {message}"));
    assert_eq!(observer.commands(), []);
    assert!(!trace.exists());
    assert!(!root.path().join("codex").exists());
}

#[tokio::test(start_paused = true)]
async fn t3_agent_home_independent_edits_boundaries_atomic_replace_and_recovery() {
    for kind in ["runner", "voice"] {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("prompts");
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join(format!("{kind}.md"));
        let path = root.path().join("trace");
        admissions(Some(root.path()), &path, 10, |index| {
            let expected = change(&file, index);
            if kind == "runner" {
                (expected, echo_config::LIVE_INSTRUCTIONS.into())
            } else {
                (echo_config::RUNNER_INSTRUCTIONS.into(), expected)
            }
        })
        .await;
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|row: &serde_json::Value| row["event"].as_str().unwrap().starts_with("prompt."))
            .collect();
        assert_eq!(rows.len(), 6);
        assert_eq!(rows.iter().filter(|row| row["event"] == "prompt.defaulted").count(), 2);
        let refused: Vec<_> = rows
            .iter()
            .filter(|row| row["event"] == "prompt.reload_refused")
            .collect();
        assert_eq!(
            refused
                .iter()
                .map(|row| row["reason"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["empty", "empty", "oversized", "invalid_utf8"]
        );
        assert!(refused.iter().all(|row| row["prompt"] == kind));
    }
}

fn change(file: &std::path::Path, index: usize) -> String {
    let prior = "\u{8a9e}\nexact \n";
    match index {
        0 | 9 => {
            if index == 9 {
                std::fs::remove_file(file).unwrap();
            }
            if file.file_stem().unwrap() == "runner" {
                echo_config::RUNNER_INSTRUCTIONS.into()
            } else {
                echo_config::LIVE_INSTRUCTIONS.into()
            }
        }
        1 => {
            std::fs::write(file, "a").unwrap();
            "a".into()
        }
        2 => {
            let replacement = file.with_extension("replacement");
            std::fs::write(&replacement, prior).unwrap();
            std::fs::rename(replacement, file).unwrap();
            prior.into()
        }
        3..=6 => {
            let bytes = match index {
                3 => vec![],
                4 => b" \n".to_vec(),
                5 => vec![b'x'; 32_769],
                _ => vec![255],
            };
            std::fs::write(file, bytes).unwrap();
            prior.into()
        }
        7 => {
            let text = "\u{8a9e}".repeat(10_922) + "ab";
            std::fs::write(file, &text).unwrap();
            text
        }
        _ => {
            std::fs::write(file, "recovered").unwrap();
            "recovered".into()
        }
    }
}

#[tokio::test(start_paused = true)]
async fn t3_agent_home_absent_section_preserves_intentional_defaults_without_trace() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    // The low-level admission port accepts explicit bases, unchanged without a home.
    admissions(None, &path, 2, |_| {
        ("configured\n\u{8a9e} \n".into(), "live\n\u{8a9e} \n".into())
    })
    .await;
    let rows = std::fs::read_to_string(path).unwrap();
    assert!(!rows.contains("prompt."));
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
