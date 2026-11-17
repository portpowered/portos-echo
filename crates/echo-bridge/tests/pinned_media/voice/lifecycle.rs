//! Repeated wakes retain one actual pinned call until idle or operator kill.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum Ending {
    Eof,
    Idle,
    Kill,
}

impl Ending {
    pub(super) fn runs(self) -> usize {
        match self {
            Self::Eof => 1,
            Self::Idle | Self::Kill => 2,
        }
    }
}

#[tokio::test]
async fn t3_pinned_voice_reuses_one_call_for_two_played_replies_then_expires() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise(Ending::Idle)))
        .await
        .unwrap();
}

#[tokio::test]
async fn t3_pinned_voice_operator_kill_stops_reused_call_before_eof() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise(Ending::Kill)))
        .await
        .unwrap();
}

pub(super) async fn replies(
    root: &Path,
    runs: usize,
    satellite: &mut TcpStream,
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    commands: &mpsc::Sender<bool>,
    clock: &mut mpsc::Receiver<()>,
) {
    for run in 1..=runs {
        if run > 1 {
            wake(satellite).await;
        }
        capture(satellite, input, output).await;
        playback(root, satellite, commands, input, output, clock).await;
        settle(root, satellite, output, run).await;
    }
}

pub(super) async fn finish(
    root: &Path,
    ending: Ending,
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) {
    assert_eq!(count(root, "live.closed"), 0);
    let milliseconds = match ending {
        Ending::Eof => return,
        Ending::Idle => 120_000,
        Ending::Kill => {
            let result = Command::new(env!("CARGO_BIN_EXE_echo-bridge"))
                .current_dir(root)
                .args(["kill", "--config", "bridge.toml"])
                .output()
                .await
                .unwrap();
            assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
            assert_eq!(result.stderr, b"");
            20
        }
    };
    input
        .write_all(format!("{}\n", json!({"action":"advance_clock", "milliseconds":milliseconds})).as_bytes())
        .await
        .unwrap();
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(
        observed,
        json!({"event":"bridge.clock_advanced","milliseconds":milliseconds})
    );
    let observed: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(observed, json!({"event":"bridge.ready"}));
    closed(root, ending);
}

pub(super) fn closed(root: &Path, ending: Ending) {
    let rows: Vec<Value> = std::fs::read_to_string(root.join("trace"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let closes: Vec<_> = rows.iter().filter(|row| row["event"] == "live.closed").collect();
    assert_eq!(closes.len(), 1);
    assert_eq!(
        closes[0]["reason"],
        match ending {
            Ending::Eof => "shutdown",
            Ending::Idle => "idle",
            Ending::Kill => "kill",
        }
    );
    assert_eq!(count(root, "kill.enabled"), usize::from(matches!(ending, Ending::Kill)));
    assert_eq!(count(root, "bridge.run_completed"), ending.runs());
}

type Screen = tokio::io::Lines<BufReader<TcpStream>>;

pub(super) async fn collected(process: tokio::process::Child, root: &Path, ending: Ending, screen: &mut Screen) {
    let result = process.wait_with_output().await.unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(result.stderr, b"");
    assert_eq!(result.stdout, b"");
    assert_eq!(next_frame(screen).await, None);
    closed(root, ending);
    assert_eq!(count(root, "bridge.error"), 0);
}

pub(super) async fn screen(root: &Path) -> Screen {
    let rows: Vec<Value> = std::fs::read_to_string(root.join("trace"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let address = rows.iter().find(|row| row["event"] == "screen.listening").unwrap()["address"]
        .as_str()
        .unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /events?token=synthetic-screen-pair HTTP/1.1\r\nHost: local\r\n\r\n")
        .await
        .unwrap();
    let mut lines = BufReader::new(stream).lines();
    assert_eq!(lines.next_line().await.unwrap().unwrap(), "HTTP/1.1 200 OK");
    lines
}

pub(super) async fn next_frame(screen: &mut Screen) -> Option<Value> {
    while let Some(line) = screen.next_line().await.unwrap() {
        if let Some(data) = line.strip_prefix("data: ") {
            let frame: echo_screen_contract::ScreenFrame = serde_json::from_str(data).unwrap();
            return Some(serde_json::to_value(frame).unwrap());
        }
    }
    None
}

pub(super) async fn presentation(screen: &mut Screen, ending: Ending) {
    let ready = json!({"state":"ready","message":"Echo Live ready"});
    let mut expected = vec![
        ready.clone(),
        json!({"state":"listening"}),
        json!({"state":"connected"}),
        json!({"state":"speaking"}),
        ready.clone(),
    ];
    if ending.runs() == 2 {
        expected.extend([json!({"state":"listening"}), json!({"state":"speaking"}), ready.clone()]);
    }
    match ending {
        Ending::Eof => (),
        Ending::Idle => expected.push(ready),
        Ending::Kill => {
            let killed = json!({"state":"error", "relink":false,
                "message":"Kill switch enabled. Run echo-bridge kill --clear with your config."});
            expected.extend([killed.clone(), killed]);
        }
    }
    for (index, state) in expected.iter().enumerate() {
        assert_eq!(next_frame(screen).await, Some(json!({"seq":index + 1,"state":state})));
    }
}
