//! Component T3: verified pin, production host/auth/media, local provider only.
use super::*;
use echo_bridge::{
    bridge_codex::{HostOptions, with_host_options},
    bridge_operator::LocalRoutes,
};
use echo_config::{CodexConfig, LiveMode};
use echo_contracts::secret::Secret;
use std::{path::Path, process::Stdio};
use tokio::{process::Command, sync::mpsc};
mod auth;
mod daily;
mod tools;
mod voice;

#[tokio::test]
async fn t3_pinned_codex_media_and_role_aware_transcripts() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(journey()))
        .await
        .unwrap();
}

async fn journey() {
    let directory = tempfile::tempdir().unwrap();

    let config = CodexConfig {
        binary: Some(binary()),
        home: Some(directory.path().join("home")),
        ..CodexConfig::default()
    };
    let mut provider = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = provider.stdin.take().unwrap();
    let mut output = BufReader::new(provider.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let origin = ready["origin"].as_str().unwrap();
    let routes = LocalRoutes::new(origin, origin).unwrap();
    let (commands, receive) = mpsc::channel(2);
    let remote = remote(&mut input, &mut output, receive);
    let client = client(&config, &routes, commands);
    let ((), (mut remote, echoed)) = tokio::join!(client, remote);
    remote.close();
    assert!((4..=8).contains(&echoed), "remote echoed {echoed} packets");
    input.write_all(b"{\"snapshot\":true}\n").await.unwrap();
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        observed,
        json!({"calls":[{
        "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
        "model":"gpt-live-1-codex", "alpha":"quicksilver=v2"}], "sidebands":1,"external":0})
    );
    // The pinned server must hand the provider the Live prompt both as session
    // instructions and as the developer item that opens the Live history.
    input.write_all(b"{\"session\":true}
").await.unwrap();
    let session: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        session,
        json!({"session":{"instructions":"Live voice prompt", "initial_items":[{"type":"message",
            "role":"developer", "content":[{"type":"input_text","text":"Live voice prompt"}]}]}})
    );
    drop(input);
    assert!(provider.wait().await.unwrap().success());
    // with_host_options has reaped its child and dropped the home lease.
    drop(
        echo_codex::codex_supervisor::Supervisor::new(echo_codex::codex_supervisor::SupervisorConfig::new(
            config.binary.unwrap(),
            config.home.unwrap(),
        ))
        .unwrap(),
    );
}

async fn client(config: &CodexConfig, routes: &LocalRoutes, commands: mpsc::Sender<bool>) {
    let key = Secret::new("dummy-local-only".to_owned());
    let result = with_host_options(
        HostOptions {
            config,
            mode: LiveMode::ApiKey,
            api_key: Some(&key),
            routes: Some(routes),
            deadline: Instant::now() + Duration::from_secs(10),
        },
        std::future::pending(),
        async |io| {
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
                    voice: None,
                    instructions: "Client audio bridge",
                    prompt: "Live voice prompt",
                    model: None,
                    effort: None,
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
        },
    )
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
                | Event::ReplyHandoff
                | Event::SpeechStarted
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
