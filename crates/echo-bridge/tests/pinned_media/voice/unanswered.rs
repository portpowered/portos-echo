//! A run the model never answers ends alone: the bridge stays up for the next wake.
use super::*;

#[tokio::test]
async fn t3_unanswered_run_ends_without_killing_the_bridge_and_next_wake_is_answered() {
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
    let (silent, first) = mpsc::channel(2);
    let (answered, second) = mpsc::channel(2);
    let (ticks, mut clock) = mpsc::channel(1);
    let device = async {
        let mut input = process.stdin.take().unwrap();
        let mut output = BufReader::new(process.stdout.take().unwrap()).lines();
        let mut satellite = listener.accept().await.unwrap().0;
        admit(&mut satellite, &mut output, &config).await;
        wake(&mut satellite).await;
        capture(&mut satellite, &mut input, &mut output).await;
        unanswered(root, &mut satellite, &mut input, &mut output, &mut clock).await;
        silent.send(RemoteCommand::Close).await.unwrap();
        silent.closed().await;
        // The same process and device connection serve the next wake on a new call.
        answered.send(RemoteCommand::Begin).await.unwrap();
        wake(&mut satellite).await;
        capture(&mut satellite, &mut input, &mut output).await;
        playback(root, &mut satellite, &answered, &mut input, &mut output, &mut clock).await;
        settle(root, &mut satellite, &mut input, &mut output, &mut clock, (1, 2)).await;
        answered.send(RemoteCommand::Close).await.unwrap();
        answered.closed().await;
    };
    let remotes = async {
        let (mut peer, echoed) = clocked_remote(&mut signal, &mut offers, first, ticks.clone()).await;
        peer.close();
        assert_eq!(echoed, 0);
        let (mut peer, echoed) = clocked_remote(&mut signal, &mut offers, second, ticks).await;
        peer.close();
        assert_eq!(echoed, REPLY_PACKETS);
    };
    tokio::join!(device, remotes);
    let result = process.wait_with_output().await.unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    traced(root);
}

/// The call connects, the model stays silent and the response window passes.
async fn unanswered(
    root: &Path,
    satellite: &mut TcpStream,
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    clock: &mut mpsc::Receiver<()>,
) {
    while count(root, "call.live_connected") == 0 {
        assert_eq!(clock.recv().await, Some(()));
        advance(input, output).await;
    }
    // examples/bridge.example.toml: task_deadline_seconds = 60.
    input
        .write_all(b"{\"action\":\"advance_clock\",\"milliseconds\":60000}\n")
        .await
        .unwrap();
    let mut wire = Vec::new();
    loop {
        let (header, _) = read_wire(satellite).await;
        let kind = header["type"].as_str().unwrap().to_owned();
        if kind == "error" {
            assert_eq!(
                header["data"],
                json!({"code":"echo-unavailable","text":"Live unavailable."})
            );
            break;
        }
        wire.push(kind);
    }
    writeln!(std::io::stdout(), "UNANSWERED wire={wire:?}").unwrap();
    // The empty reply is flushed, then the run is ended on the device.
    assert_eq!(wire, ["audio-start", "hassmic-flush", "hassmic-end", "hassmic-end"]);
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(observed, json!({"event":"bridge.clock_advanced","milliseconds":60000}));
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(observed, json!({"event":"bridge.ready"}));
    assert_eq!(count(root, "bridge.run_completed"), 0);
}

fn traced(root: &Path) {
    let rows: Vec<Value> = std::fs::read_to_string(root.join("trace"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let named = |event: &str| -> Vec<(Value, Value)> {
        rows.iter()
            .filter(|row| row["event"] == event)
            .map(|row| (row["run_id"].clone(), row["reason"].clone()))
            .collect()
    };
    assert_eq!(named("run.ended"), [(json!("run-1"), json!("no_response"))]);
    assert_eq!(
        named("live.closed"),
        [(Value::Null, json!("no_response")), (Value::Null, json!("shutdown"))]
    );
    assert_eq!(named("bridge.error"), []);
    assert_eq!(named("bridge.run_completed"), [(json!("run-2"), Value::Null)]);
    assert_eq!(count(root, "call.live_connected"), 2);
    assert_eq!(count(root, "bridge.ready"), 2);
    let replies: Vec<_> = rows.iter().filter(|row| row["event"] == "reply.played").collect();
    assert_eq!(replies.len(), 1);
    assert_eq!(
        (&replies[0]["run_id"], &replies[0]["outcome"]),
        (&json!("run-2"), &json!("played"))
    );
}
