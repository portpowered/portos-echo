//! First wake with no session: the request is spoken while the call is set up.
use super::*;
use echo_live::AudioFrame;

/// 0.8 s of speech, all of it sent before the remote answers the offer.
const EARLY_CHUNKS: usize = 40;
/// The media encoder's 16 to 48 kHz converter holds the final frame back.
const HEARD_PACKETS: usize = EARLY_CHUNKS - 1;

#[tokio::test]
async fn t3_first_wake_speech_before_live_reaches_the_model_and_is_answered() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise()))
        .await
        .unwrap();
}

async fn exercise() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = configure(root, listener.local_addr().unwrap().port());
    let mut provider = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut signal = provider.stdin.take().unwrap();
    let mut offers = BufReader::new(provider.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&offers.next_line().await.unwrap().unwrap()).unwrap();
    let mut process = launch(root, ready["origin"].as_str().unwrap());
    let mut input = process.stdin.take().unwrap();
    let mut output = BufReader::new(process.stdout.take().unwrap()).lines();
    let mut satellite = listener.accept().await.unwrap().0;
    admit(&mut satellite, &mut output, &config).await;
    wake(&mut satellite).await;
    // The offer proves call setup began. The user is already talking: the whole
    // request reaches the bridge before the remote answers and media is live.
    let request: Value = serde_json::from_str(&offers.next_line().await.unwrap().unwrap()).unwrap();
    speak(&mut satellite).await;
    let mut peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let answer = peer.answer(request["offer"].as_str().unwrap()).await.unwrap();
    signal
        .write_all(format!("{}\n", json!({"answer":answer})).as_bytes())
        .await
        .unwrap();
    let reply = hear(&mut peer, &mut input, &mut output).await;
    let (commands, receive) = mpsc::channel(2);
    let (ticks, mut clock) = mpsc::channel(1);
    let device = async {
        commands.send(RemoteCommand::Begin).await.unwrap();
        advance(&mut input, &mut output).await;
        playback(root, &mut satellite, &commands, &mut input, &mut output, &mut clock).await;
        settle(root, &mut satellite, &mut input, &mut output, &mut clock, 1).await;
        commands.send(RemoteCommand::Close).await.unwrap();
        commands.closed().await;
    };
    let ((), mut peer) = tokio::join!(device, answering(peer, &mut signal, receive, ticks, reply));
    peer.close();
    drop(input);
    let result = process.wait_with_output().await.unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    captured(root);
}

/// One uninterrupted utterance, then the device's end of capture.
async fn speak(satellite: &mut TcpStream) {
    let format = json!({"rate":16000,"width":2,"channels":1});
    send(satellite, "audio-start", format.clone(), vec![]).await;
    let wave: [i16; 16] = [
        0, 6000, 11000, 14500, 16000, 14500, 11000, 6000, 0, -6000, -11000, -14500, -16000, -14500, -11000, -6000,
    ];
    for _ in 0..EARLY_CHUNKS {
        let pcm = (0..320).flat_map(|n| wave[n % 16].to_le_bytes()).collect();
        send(satellite, "audio-chunk", format.clone(), pcm).await;
    }
    send(satellite, "audio-stop", json!({}), vec![]).await;
}

/// The model answers only a request it heard in full. Each remote media step
/// releases one bridge clock step, as setup needs. Media arrival is the
/// readiness signal; the watchdog only turns lost speech into a counted failure.
async fn hear(
    peer: &mut LivePeer,
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> Vec<AudioFrame> {
    let mut heard = Vec::new();
    let complete = async {
        while heard.len() < HEARD_PACKETS {
            step(peer).await;
            while let Some(frame) = peer.pop_audio() {
                heard.push(frame);
            }
            advance(input, output).await;
        }
    };
    let lost = tokio::time::timeout(Duration::from_secs(5), complete).await.is_err();
    writeln!(std::io::stdout(), "FIRST-WAKE heard={} of {HEARD_PACKETS}", heard.len()).unwrap();
    assert_eq!((heard.len(), lost), (HEARD_PACKETS, false), "speech packets heard");
    heard.truncate(REPLY_PACKETS);
    heard
}

async fn answering(
    mut peer: LivePeer,
    signal: &mut tokio::process::ChildStdin,
    mut commands: mpsc::Receiver<RemoteCommand>,
    ticks: mpsc::Sender<()>,
    reply: Vec<AudioFrame>,
) -> LivePeer {
    let mut reply = reply.into_iter();
    loop {
        tokio::select! {
            () = step(&mut peer) => {
                assert_eq!(peer.pop_audio().map(|frame| frame.payload.len()), None, "speech after end of capture");
                let _ = ticks.try_send(());
            },
            command = commands.recv() => match command {
                Some(RemoteCommand::Begin) => for frame in reply.by_ref() {
                    peer.send_audio(frame, Instant::now().into_std()).await.unwrap();
                },
                Some(RemoteCommand::Transcript) => signal.write_all(b"{\"transcripts\":true}\n").await.unwrap(),
                value => {
                    assert_eq!(value, Some(RemoteCommand::Close));
                    return peer;
                },
            },
        }
    }
}

fn captured(root: &Path) {
    let rows: Vec<Value> = std::fs::read_to_string(root.join("trace"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let microphone: Vec<_> = rows.iter().filter(|row| row["event"] == "mic.completed").collect();
    assert_eq!(microphone.len(), 1);
    assert_eq!(microphone[0]["count"], EARLY_CHUNKS);
    assert_eq!(microphone[0]["received_chunks"], EARLY_CHUNKS);
    assert_eq!(microphone[0]["dropped_chunks"], 0);
    assert_eq!(count(root, "mic.uplink_dropped"), 0);
    let replies: Vec<_> = rows.iter().filter(|row| row["event"] == "reply.played").collect();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["outcome"], "played");
    assert_eq!(count(root, "bridge.error"), 0);
}
