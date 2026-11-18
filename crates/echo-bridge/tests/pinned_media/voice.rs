//! T3 public executable: pin + real media + loopback Wyoming, controlled clock.
use super::*;
use std::io::Write as _;
pub(super) mod lifecycle;
use echo_contracts::wyoming::{Event as WireEvent, Header, encode};
use lifecycle::Ending;
#[derive(Debug, PartialEq)]
pub(super) enum RemoteCommand {
    Begin,
    Transcript,
    Close,
}

// Five Opus packets yield three paced PCM frames before both converters flush.
// Receiving the third frame is the causal barrier before transcript completion.
const REPLY_PACKETS: usize = 5;

use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
};

#[tokio::test]
async fn t3_pinned_voice_executable_waits_for_played_and_releases_home() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise(Ending::Eof)))
        .await
        .unwrap();
}

async fn exercise(ending: Ending) {
    let directory = tempfile::tempdir().unwrap();
    private_parent(directory.path());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = configure(directory.path(), listener.local_addr().unwrap().port());
    let mut provider = Command::new(if cfg!(windows) { "python" } else { "python3" })
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
    let (commands, receive) = mpsc::channel(2);
    let (ticks, clock) = mpsc::channel(1);
    let ((), (mut remote, echoed)) = tokio::join!(
        executable(directory.path(), &config, origin, listener, (commands, clock), ending),
        clocked_remote(&mut input, &mut output, receive, ticks)
    );
    remote.close();
    assert!(
        echoed == REPLY_PACKETS * ending.runs(),
        "remote echoed {echoed} packets"
    );
    input.write_all(b"{\"snapshot\":true}\n").await.unwrap();
    let snapshot: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        snapshot,
        json!({"calls":[{
        "path":"/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas",
        "model":"gpt-live-1-codex", "alpha":"quicksilver=v2"}],"sidebands":1,"external":0})
    );
    input.write_all(b"{\"transcript_count\":true}\n").await.unwrap();
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(observed, json!({"transcripts": ending.runs()}));
    drop(input);
    assert!(provider.wait().await.unwrap().success());
    // The actual executable reaped its supervisor and released exclusive ownership.
    drop(
        echo_codex::codex_supervisor::Supervisor::new(echo_codex::codex_supervisor::SupervisorConfig::new(
            config.binary.unwrap(),
            config.home.unwrap(),
        ))
        .unwrap(),
    );
}

pub(super) fn configure(root: &Path, port: u16) -> CodexConfig {
    let config = CodexConfig {
        binary: Some(binary()),
        home: Some(root.join("home")),
        ..CodexConfig::default()
    };
    std::fs::write(root.join("key"), "dummy-local-only\n").unwrap();
    std::fs::write(root.join("screen-token"), "synthetic-screen-pair").unwrap();
    let template = include_str!("../../../../examples/bridge.example.toml");
    let start = template.find("[[mcp.server]]").unwrap();
    let end = start + template[start..].find("[screen]").unwrap();
    let text = format!("{}[mcp]\nserver = []\n\n{}", &template[..start], &template[end..])
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("wyoming_port = 16700", &format!("wyoming_port = {port}"))
        .replace("mode = \"chatgpt\"", "mode = \"api_key\"")
        .replace("credentials/live.json", "key")
        .replace("voice = \"default\"\n", "")
        .replace("0.0.0.0:16900", "127.0.0.1:0") // lint-rules: allow-literal replace inert example listener with loopback
        .replace("credentials/screen-pairing.txt", "screen-token");
    std::fs::write(
        root.join("bridge.toml"),
        format!(
            "{text}\n[codex]\nhome = '{}'\nbinary = '{}'\n",
            config.home.as_ref().unwrap().display(),
            config.binary.as_ref().unwrap().display()
        ),
    )
    .unwrap();
    config
}

async fn executable(
    root: &Path,
    config: &CodexConfig,
    origin: &str,
    listener: TcpListener,
    channels: (mpsc::Sender<RemoteCommand>, mpsc::Receiver<()>),
    ending: Ending,
) {
    let (commands, mut clock) = channels;
    let mut process = launch(root, origin);
    let mut input = process.stdin.take().unwrap();
    let mut output = BufReader::new(process.stdout.take().unwrap()).lines();
    let mut satellite = listener.accept().await.unwrap().0;
    admit(&mut satellite, &mut output, config).await;
    let mut screen = lifecycle::screen(root).await;
    wake(&mut satellite).await;
    lifecycle::replies(
        root,
        ending.runs(),
        &mut satellite,
        &mut input,
        &mut output,
        &commands,
        &mut clock,
    )
    .await;
    statistics(root, ending.runs());
    commands.send(RemoteCommand::Close).await.unwrap();
    commands.closed().await;
    lifecycle::finish(root, ending, &mut input, &mut output).await;
    lifecycle::presentation(&mut screen, ending).await;
    drop(input);
    lifecycle::collected(process, root, ending, &mut screen).await;
}

async fn settle(
    root: &Path,
    satellite: &mut TcpStream,
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    clock: &mut mpsc::Receiver<()>,
    run: usize,
) {
    assert_eq!(count(root, "reply.played"), run - 1);
    assert_eq!(count(root, "bridge.run_completed"), run - 1);
    send(satellite, "played", json!({}), vec![]).await;
    let mut pending = false;
    let mut completed = false;
    // Played can precede the delayed mouth boundary. Keep the same clock moving,
    // and drain its receipt even when completion is published before the receipt.
    while !completed || pending {
        tokio::select! {
            line = output.next_line() => {
                let observed: Value = serde_json::from_str(&line.unwrap().unwrap()).unwrap();
                settle_receipt(&observed, &mut pending, &mut completed, run);
            },
            tick = clock.recv(), if !pending && !completed => {
                assert_eq!(tick, Some(()));
                input.write_all(b"{\"action\":\"advance_playout\",\"milliseconds\":20}\n").await.unwrap();
                pending = true;
            },
        }
    }
    assert_eq!(count(root, "reply.played"), run);
}

fn settle_receipt(observed: &Value, pending: &mut bool, completed: &mut bool, run: usize) {
    if observed["event"] == "bridge.clock_advanced" {
        assert!(*pending);
        assert_eq!(observed["milliseconds"], 20);
        assert!(observed["epoch_ms"].as_u64().is_some());
        writeln!(std::io::stdout(), "PINNED-CLOCK {observed}").unwrap();
        *pending = false;
    } else {
        assert!(!*completed);
        assert_eq!(
            observed,
            &json!({"event":"bridge.run_completed", "run_id":format!("run-{run}")})
        );
        *completed = true;
    }
}

pub(super) fn launch(root: &Path, origin: &str) -> tokio::process::Child {
    Command::new(env!("CARGO_BIN_EXE_echo-bridge"))
        .current_dir(root)
        .args([
            "--config",
            "bridge.toml",
            "--trace",
            "trace",
            "--health-listen",
            "127.0.0.1:0",
            "--live-loopback",
            origin,
            "--model-loopback",
            origin,
            "--sim-clock",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

async fn admit(
    satellite: &mut TcpStream,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    config: &CodexConfig,
) {
    assert_eq!(read_wire(satellite).await.0["type"], "run-satellite");
    assert_eq!(read_wire(satellite).await.0["type"], "describe");
    send(satellite, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await;
    assert_eq!(
        output.next_line().await.unwrap().unwrap(),
        r#"{"event":"bridge.ready"}"#
    );
    // A second owner cannot acquire the active executable's home.
    assert!(matches!(
        echo_codex::codex_supervisor::Supervisor::new(echo_codex::codex_supervisor::SupervisorConfig::new(
            config.binary.clone().unwrap(),
            config.home.clone().unwrap()
        )),
        Err(echo_codex::codex_supervisor::ProcessError::PrivateHome)
    ));
}

async fn wake(satellite: &mut TcpStream) {
    let row: Value = serde_json::from_str(
        include_str!("../../../../tests/fixtures/wyoming/wake-word.jsonl")
            .lines()
            .nth(1)
            .unwrap(),
    )
    .unwrap();
    send(satellite, "run-pipeline", row["header"]["data"].clone(), vec![]).await;
}

async fn capture(
    satellite: &mut TcpStream,
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) {
    let format = json!({"rate":16000,"width":2,"channels":1});
    send(satellite, "audio-start", format.clone(), vec![]).await;
    let wave: [i16; 16] = [
        0, 6000, 11000, 14500, 16000, 14500, 11000, 6000, 0, -6000, -11000, -14500, -16000, -14500, -11000, -6000,
    ];
    for _ in 0..8 {
        let pcm = (0..320).flat_map(|n| wave[n % 16].to_le_bytes()).collect();
        send(satellite, "audio-chunk", format.clone(), pcm).await;
    }
    send(satellite, "audio-stop", json!({}), vec![]).await;
    // Eight capture frames fit the uplink lead. Admit the finite utterance at
    // one epoch so its echoed reply cannot acquire fixture-induced gaps.
    advance(input, output).await;
}

async fn playback(
    root: &Path,
    satellite: &mut TcpStream,
    commands: &mpsc::Sender<RemoteCommand>,
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    clock: &mut mpsc::Receiver<()>,
) {
    let completed = count(root, "bridge.run_completed");
    let mut frames = 0;
    let mut pcm = Vec::new();
    loop {
        // All finite reply frames fit the configured 200 ms lead. Once the
        // first frame arrives, collect the rest and transcript-flushed tail at
        // this epoch; the external pin's scheduling is not a playout gap.
        let (header, bytes) = if frames == 0 {
            clocked_read(satellite, input, output, clock).await
        } else {
            read_wire(satellite).await
        };
        match header["type"].as_str().unwrap() {
            "describe" => send(satellite, "info", json!({"satellite":{"name":"synthetic"}}), vec![]).await,
            "audio-start" => assert_eq!(header["data"], json!({"rate":24000,"width":2,"channels":1})),
            "audio-chunk" => {
                assert_eq!(bytes.len(), 960);
                frames += 1;
                pcm.extend(bytes);
                if frames == 3 {
                    commands.send(RemoteCommand::Transcript).await.unwrap();
                }
            }
            "audio-stop" => break,
            other => assert_eq!(other, "expected reply audio", "unexpected Wyoming event"),
        }
    }
    assert!((3..=8).contains(&frames), "reply frames {frames}");
    let energy: i64 = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i64::from(i16::from_le_bytes(*b)).pow(2))
        .sum();
    assert!(energy > 1440 * 1000_i64.pow(2), "reply PCM energy {energy}");
    assert_eq!(count(root, "bridge.run_completed"), completed);
}

fn count(root: &Path, event: &str) -> usize {
    std::fs::read_to_string(root.join("trace"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["event"] == event)
        .count()
}

async fn send(stream: &mut TcpStream, kind: &str, data: Value, payload: Vec<u8>) {
    stream
        .write_all(&encode(&WireEvent {
            header: Header {
                r#type: kind.into(),
                data: Some(data),
                data_length: 0,
                payload_length: payload.len(),
            },
            data: None,
            payload,
        }))
        .await
        .unwrap();
}

async fn read_wire(stream: &mut TcpStream) -> (Value, Vec<u8>) {
    let mut bytes = vec![];
    loop {
        let byte = stream.read_u8().await.unwrap();
        bytes.push(byte);
        assert!(bytes.len() < 8192);
        if byte == b'\n' {
            break;
        }
    }
    let header: Value = serde_json::from_slice(&bytes).unwrap();
    let length = |key: &str| usize::try_from(header[key].as_u64().unwrap_or(0)).unwrap();
    let mut data = vec![0; length("data_length")];
    stream.read_exact(&mut data).await.unwrap();
    let mut payload = vec![0; length("payload_length")];
    stream.read_exact(&mut payload).await.unwrap();
    (header, payload)
}

async fn clocked_remote(
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    mut commands: mpsc::Receiver<RemoteCommand>,
    ticks: mpsc::Sender<()>,
) -> (LivePeer, usize) {
    let request: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let mut peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let offer = request["offer"].as_str().unwrap();
    loopback_offer(offer);
    let answer = peer.answer(offer).await.unwrap();
    input
        .write_all(format!("{}\n", json!({"answer":answer})).as_bytes())
        .await
        .unwrap();
    let mut echoed = 0;
    let mut remaining = 0;
    loop {
        echo_reply(&mut peer, &mut remaining, &mut echoed).await;
        tokio::select! {
            () = step(&mut peer) => { let _ = ticks.try_send(()); },
            command = commands.recv() => match command {
                Some(RemoteCommand::Begin) => { assert_eq!(remaining, 0); remaining = REPLY_PACKETS; },
                Some(RemoteCommand::Transcript) => { assert_eq!(remaining, 0); input.write_all(b"{\"transcripts\":true}\n").await.unwrap(); },
                value => {assert_eq!(value,Some(RemoteCommand::Close)); return (peer, echoed);},
            },
        }
    }
}

// Retain a partially read header/body across clock commands.
async fn clocked_read(
    satellite: &mut TcpStream,
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    clock: &mut mpsc::Receiver<()>,
) -> (Value, Vec<u8>) {
    let wire = read_wire(satellite);
    tokio::pin!(wire);
    loop {
        tokio::select! {
            value = &mut wire => return value,
            tick = clock.recv() => {
                assert_eq!(tick, Some(()));
                advance(input, output).await;
            },
        }
    }
}

async fn advance(
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) {
    input
        .write_all(b"{\"action\":\"advance_playout\",\"milliseconds\":20}\n")
        .await
        .unwrap();
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(observed["event"], "bridge.clock_advanced");
    assert_eq!(observed["milliseconds"], 20);
    assert!(observed["epoch_ms"].as_u64().is_some());
    writeln!(std::io::stdout(), "PINNED-CLOCK {observed}").unwrap();
}

fn loopback_offer(offer: &str) {
    let candidates: Vec<_> = offer.lines().filter(|line| line.starts_with("a=candidate:")).collect();
    assert_ne!(candidates, Vec::<&str>::new());
    for candidate in candidates {
        assert_eq!(candidate.split_whitespace().nth(4), Some("127.0.0.1"));
    }
}

fn statistics(root: &Path, runs: usize) {
    let rows: Vec<Value> = std::fs::read_to_string(root.join("trace"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    lifecycle::setup_stages(&rows);
    let microphone: Vec<_> = rows.iter().filter(|row| row["event"] == "mic.completed").collect();
    assert_eq!(microphone.len(), runs);
    for row in microphone {
        assert_eq!(row["count"], 8);
        assert_eq!(row["received_chunks"], 8);
        assert_eq!(row["dropped_chunks"], 0);
        assert_eq!(row["tail_samples"], 0);
    }
    let replies: Vec<_> = rows.iter().filter(|row| row["event"] == "reply.played").collect();
    assert_eq!(replies.len(), runs);
    for row in replies {
        assert_eq!(row["outcome"], "played");
        assert_eq!(row["dropped_frames"], 0);
        assert_eq!(row["frames"], row["received_frames"]);
    }
}

async fn echo_reply(peer: &mut LivePeer, remaining: &mut usize, echoed: &mut usize) {
    if peer.ready() {
        while let Some(frame) = peer.pop_audio() {
            if *remaining > 0 {
                peer.send_audio(frame, Instant::now().into_std()).await.unwrap();
                *echoed += 1;
                *remaining -= 1;
            }
        }
    }
}
