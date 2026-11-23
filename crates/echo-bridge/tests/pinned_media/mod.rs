//! Component T3: verified pin, production host/auth/media, local provider only.
use super::*;
use echo_bridge::{
    bridge_codex::{HostOptions, with_host_options},
    bridge_operator::LocalRoutes,
};
use echo_codex::{
    codex_protocol::{Message, RealtimeStart},
    codex_supervisor::{Supervisor, SupervisorConfig},
};
use echo_config::{CodexConfig, LiveMode};
use echo_contracts::secret::Secret;
use std::{path::Path, process::Stdio};
use tokio::{
    io::Lines,
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::mpsc,
};
mod auth;
mod daily;
mod tools;
mod voice;

#[tokio::test]
async fn t3_pinned_codex_media_and_role_aware_transcripts() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(journey(None, "cove")))
        .await
        .unwrap();
}

#[tokio::test]
async fn t3_pinned_codex_himari_negotiates_v3_media() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(journey(Some("himari"), "himari")))
        .await
        .unwrap();
}

#[tokio::test]
async fn t3_pinned_codex_lists_v3_voices() {
    use std::io::Write as _;
    let directory = tempfile::tempdir().unwrap();
    let mut supervisor = Supervisor::new(SupervisorConfig::new(binary(), directory.path().join("home"))).unwrap();
    supervisor.start().await.unwrap();
    let io = supervisor.io().unwrap();
    let request = io.begin_request("thread/realtime/listVoices", &json!({})).unwrap();
    let result = io.wait_reply(&request).await.unwrap();
    supervisor.shutdown().await.unwrap();
    // No release is pinned, so a newer one may list more voices. The witness is
    // the list itself (rust-v0.171.0 lists 26); the claims are the names Echo
    // documents and the v2-only name it refuses.
    writeln!(std::io::stdout(), "{}", json!({"v3_voices": result["voices"]["v3"]})).unwrap();
    let listed: Vec<&str> = result["voices"]["v3"]
        .as_array()
        .unwrap()
        .iter()
        .map(|voice| voice.as_str().unwrap())
        .collect();
    for voice in echo_config::realtime_voices()
        .filter(|voice| *voice != "default")
        .chain(["miko", "himari", "hanul", "leher"])
    {
        assert!(listed.contains(&voice), "{voice} is not listed: {listed:?}");
    }
    assert!(!listed.contains(&"alloy"), "{listed:?}");
}

#[tokio::test]
async fn t3_pinned_codex_refuses_alloy_before_call_or_sideband() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(refuse_alloy()))
        .await
        .unwrap();
}

async fn refuse_alloy() {
    let directory = tempfile::tempdir().unwrap();
    let config = pinned_config(directory.path());
    let (mut provider, mut input, mut output, routes) = start_provider().await;
    let key = Secret::new("dummy-local-only".to_owned());
    let result = with_host_options(
        host_options(&config, &routes, &key),
        std::future::pending(),
        async |io| {
            let id = io
                .begin_request("thread/start", &json!({"ephemeral":true,"environments":[]}))
                .unwrap();
            let started = io.wait_reply(&id).await.unwrap();
            let thread_id = started["thread"]["id"].as_str().unwrap();
            let mut params = serde_json::to_value(RealtimeStart::new(thread_id, "v=0\r\n")).unwrap();
            params["voice"] = "alloy".into();
            let id = io.begin_request("thread/realtime/start", &params).unwrap();
            assert_eq!(io.wait_reply(&id).await.unwrap(), json!({}));
            loop {
                if let Message::Notification { method, params, .. } = io.next_event().await.unwrap() {
                    match method.as_str() {
                        "thread/realtime/error" => {
                            assert_eq!(params["threadId"], thread_id);
                            assert!(
                                params["message"]
                                    .as_str()
                                    .unwrap()
                                    .contains("realtime voice `alloy` is not supported for v3; supported voices:"),
                                "{params}"
                            );
                            break;
                        }
                        other => assert!(
                            !matches!(other, "thread/realtime/started" | "thread/realtime/sdp"),
                            "alloy started: {params}"
                        ),
                    }
                }
            }
            Ok(())
        },
    )
    .await;
    assert_eq!(result, Ok(()));
    input.write_all(b"{\"snapshot\":true}\n").await.unwrap();
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(observed, json!({"calls":[],"sidebands":0,"external":0}));
    drop(input);
    assert!(provider.wait().await.unwrap().success());
}

fn pinned_config(root: &Path) -> CodexConfig {
    CodexConfig {
        binary: Some(binary()),
        home: Some(root.join("home")),
        ..CodexConfig::default()
    }
}

async fn start_provider() -> (Child, ChildStdin, Lines<BufReader<ChildStdout>>, LocalRoutes) {
    let mut provider = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let input = provider.stdin.take().unwrap();
    let mut output = BufReader::new(provider.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let origin = ready["origin"].as_str().unwrap();
    let routes = LocalRoutes::new(origin, origin).unwrap();
    (provider, input, output, routes)
}

async fn journey(voice: Option<&str>, provider_voice: &str) {
    let directory = tempfile::tempdir().unwrap();
    let config = pinned_config(directory.path());
    let (mut provider, mut input, mut output, routes) = start_provider().await;
    let (commands, receive) = mpsc::channel(2);
    let remote = remote(&mut input, &mut output, receive);
    let client = client(&config, &routes, commands, voice);
    let ((), (mut remote, echoed)) = tokio::join!(client, remote);
    remote.close();
    assert!((4..=8).contains(&echoed), "remote echoed {echoed} packets");
    input.write_all(b"{\"snapshot\":true}\n").await.unwrap();
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        observed,
        json!({"calls":[{
        "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
        "model":"gpt-live-1.1-codex", "alpha":"quicksilver=v2"}], "sidebands":1,"external":0})
    );
    call_session(&mut input, &mut output, provider_voice).await;
    drop(input);
    assert!(provider.wait().await.unwrap().success());
    // with_host_options has reaped its child and dropped the home lease.
    drop(Supervisor::new(SupervisorConfig::new(config.binary.unwrap(), config.home.unwrap())).unwrap());
}

/// What the pinned server put in the call session it sent the provider.
async fn call_session(input: &mut ChildStdin, output: &mut Lines<BufReader<ChildStdout>>, provider_voice: &str) {
    let mut ask = async |command: &str| -> Value {
        input.write_all(format!("{command}\n").as_bytes()).await.unwrap();
        serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap()
    };
    assert_eq!(
        ask(r#"{"call_voices":true}"#).await,
        json!({"call_voices":[provider_voice]})
    );
    // The pinned server must hand the provider the Live prompt both as session
    // instructions and as the developer item that opens the Live history.
    assert_eq!(
        ask(r#"{"session":true}"#).await,
        json!({"session":{"instructions":"Live voice prompt", "initial_items":[{"type":"message",
            "role":"developer", "content":[{"type":"input_text","text":"Live voice prompt"}]}]}})
    );
    // Owned by Codex/OpenAI, not the bridge: the V3 call session Codex sends
    // for the bridge's start sets no turn detection, interruption or input
    // audio. Barge-in is the model's own judgment; a release that configures
    // it changes what the bridge's local cut sits on (ADR 0024, 2026-11-21).
    assert_eq!(
        ask(r#"{"turn_taking":true}"#).await,
        json!({"turn_taking":{"paths":[], "audio":["output"]}})
    );
}

fn host_options<'a>(config: &'a CodexConfig, routes: &'a LocalRoutes, key: &'a Secret<String>) -> HostOptions<'a> {
    HostOptions {
        config,
        mode: LiveMode::ApiKey,
        api_key: Some(key),
        routes: Some(routes),
        deadline: Instant::now() + Duration::from_secs(10),
    }
}

async fn client(config: &CodexConfig, routes: &LocalRoutes, commands: mpsc::Sender<bool>, voice: Option<&str>) {
    let key = Secret::new("dummy-local-only".to_owned());
    let result = with_host_options(host_options(config, routes, &key), std::future::pending(), async |io| {
        let peer = Peer::create(PeerConfig {
            interface_allowlist: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            allow_loopback: true,
            queue_capacity: NonZeroUsize::new(32).unwrap(),
            ..PeerConfig::default()
        })
        .await
        .unwrap();
        let mut call = MediaCall::new(io, peer);
        call.start(
            &Options {
                voice,
                instructions: "Client audio bridge",
                prompt: "Live voice prompt",
                model: None,
                effort: None,
                speed: echo_config::DelegateSpeed::Standard,
                time_zone: None,
                web_search: echo_config::WebSearch::default(),
            },
            vec![],
            Instant::now() + Duration::from_secs(10),
        )
        .await?;
        audio_round_trip(&mut call).await;
        commands.send(true).await.unwrap();
        transcripts(&mut call).await;
        commands.send(false).await.unwrap();
        commands.closed().await;
        call.close(Instant::now() + Duration::from_secs(3)).await?;
        Ok(())
    })
    .await;
    assert_eq!(result, Ok(()));
}

async fn transcripts<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    call: &mut MediaCall<'_, R, W>,
) {
    let mut transcript = vec![];
    loop {
        match call.next().await.unwrap() {
            MediaEvent::Control(Event::Transcript { role, text, done }) => transcript.push(json!([role, text, done])),
            MediaEvent::Control(Event::ReplyDone { text }) => {
                transcript.push(json!(["assistant", text, true]));
                break;
            }
            MediaEvent::Control(Event::Other) | MediaEvent::Audio(_) => (),
            MediaEvent::Control(
                Event::Closed
                | Event::Tool { .. }
                | Event::TurnStarted { .. }
                | Event::TurnCompleted { .. }
                | Event::ReplyHandoff(_)
                | Event::SpeechStarted
                | Event::SpeechStopped
                | Event::Session(_),
            ) => {
                transcript.push(json!(["unexpected pinned-server event"]));
                break;
            }
        }
    }
    assert_eq!(
        transcript,
        vec![
            json!(["user", "ping", false]),
            json!(["user", "ping", true]),
            json!(["assistant", "echo-ok", false]),
            json!(["assistant", "echo-ok", true])
        ]
    );
}

async fn remote(
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    mut commands: mpsc::Receiver<bool>,
) -> (LivePeer, usize) {
    let request: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let mut peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let answer = peer.answer(request["offer"].as_str().unwrap()).await.unwrap();
    input
        .write_all(format!("{}\n", json!({"answer":answer})).as_bytes())
        .await
        .unwrap();
    let mut echoed = 0;
    loop {
        echoed += echo_ready(&mut peer).await;
        tokio::select! {
            () = step(&mut peer) => (),
            observation = output.next_line() => {
                assert_eq!(observation.unwrap(), None, "provider rejected the pinned request");
                return (peer, echoed);
            },
            command = commands.recv() => match command {
                Some(true) => input.write_all(b"{\"transcripts\":true}\n").await.unwrap(),
                command => {
                    assert_eq!(command, Some(false), "client ended without media cleanup");
                    return (peer, echoed);
                },
            },
        }
    }
}

async fn echo_ready(peer: &mut LivePeer) -> usize {
    let mut echoed = 0;
    if peer.ready() {
        while let Some(frame) = peer.pop_audio() {
            peer.send_audio(frame, Instant::now().into_std()).await.unwrap();
            echoed += 1;
        }
    }
    echoed
}

fn binary() -> std::path::PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut command = {
        let mut command = std::process::Command::new("sh");
        command.arg(root.join("scripts/codex-app-server.sh"));
        command
    };
    let output = command.env("ECHO_CODEX_OFFLINE", "1").output().unwrap();
    assert!(output.status.success(), "offline pinned-binary resolution failed");
    String::from_utf8(output.stdout).unwrap().trim().into()
}
mod startup;
