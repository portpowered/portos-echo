use super::*;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

// T1: real terminal arbitration and trace projection, memory-only cleanup
// barriers and a private durable marker. No timer or socket is needed.
#[tokio::test(start_paused = true)]
async fn wake_earcon_kill_shutdown_ordering() {
    for order in [
        "kill_first",
        "eof_first",
        "both_ready",
        "kill_cleared",
        "shutdown",
        "failure",
        "mute",
    ] {
        terminal_order(order).await;
    }
}

async fn terminal_order(order: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("kill");
    let trace_path = directory.path().join("trace");
    let mut trace = Trace::open(&trace_path, "api_key").unwrap();
    let screen = Screen::bind(None).await.unwrap();
    let health = Health::offline();
    let mut view = View::new(&screen, &health, &mut trace);
    let termination = super::super::voice_kill::Termination::default();
    let requested = Cell::new(false);
    let (entered, cleanup_entered) = tokio::sync::oneshot::channel();
    let (release, cleanup_released) = tokio::sync::oneshot::channel();
    let work = async {
        observe_terminal(&termination, &mut view, super::super::ServeNotice::Listening);
        let result = interrupt(order, &path, &termination, &mut view).await;
        requested.set(order == "eof_first" || order == "both_ready" || order == "shutdown");
        termination.returned(&result, requested.get());
        entered.send(()).unwrap();
        cleanup_released.await.unwrap();
        termination.finish(result, requested.get(), Some(&path), &mut |notice| view.observe(notice))
    };
    let signals = async {
        cleanup_entered.await.unwrap();
        if order == "eof_first" {
            std::fs::write(&path, b"").unwrap();
        } else if order == "kill_cleared" {
            std::fs::remove_file(&path).unwrap();
        }
        requested.set(true);
        release.send(()).unwrap();
    };
    let (result, ()) = tokio::join!(work, signals);
    assert_eq!(
        result,
        if order == "shutdown" {
            Ok(())
        } else if order == "failure" {
            Err(Error::Wyoming)
        } else {
            Err(Error::Control)
        },
        "{order}"
    );
    terminal_receipts(&trace_path, order);
    // Clear/recovery starts a new run rather than poisoning future EOF.
    let _ = std::fs::remove_file(&path);
    observe_terminal(&termination, &mut view, super::super::ServeNotice::Listening);
    assert_eq!(
        termination.finish(Err(Error::Control), true, Some(&path), &mut |notice| view
            .observe(notice)),
        Ok(())
    );
}

fn terminal_receipts(trace_path: &Path, order: &str) {
    let rows: Vec<Value> = std::fs::read_to_string(trace_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        rows.iter().filter(|row| row["event"] == "kill.enabled").count(),
        usize::from(matches!(
            order,
            "kill_first" | "eof_first" | "both_ready" | "kill_cleared"
        )),
        "{order}"
    );
    assert!(!rows.iter().any(|row| matches!(
        row["event"].as_str(),
        Some("wake.earcon_played" | "reply.played" | "bridge.run_completed")
    )));
}

fn observe_terminal(
    termination: &super::super::voice_kill::Termination,
    view: &mut View<'_>,
    notice: super::super::ServeNotice,
) {
    termination.observe(&notice);
    view.observe(notice).unwrap();
}

async fn interrupt(
    order: &str,
    path: &Path,
    termination: &super::super::voice_kill::Termination,
    view: &mut View<'_>,
) -> Result<(), Error> {
    use super::super::{Notice, ServeNotice, SessionNotice};
    if matches!(order, "kill_first" | "kill_cleared" | "both_ready") {
        std::fs::write(path, b"").unwrap();
        let killed = Cell::new(false);
        let gate = super::super::voice_kill::Gate::new(Some(path));
        gate.cancel(
            async {
                if order != "both_ready" {
                    std::future::pending::<()>().await;
                }
            },
            &killed,
        )
        .await;
        assert!(killed.get(), "{order}: ready kill must beat EOF");
        if order == "kill_first" {
            observe_terminal(termination, view, ServeNotice::Kill(true));
        } else {
            termination.trip();
        }
    } else if order == "mute" {
        observe_terminal(
            termination,
            view,
            ServeNotice::Session(SessionNotice::Run(Notice::Device(echo_link::LinkEvent::Mute(true)))),
        );
    }
    Err(if order == "failure" {
        Error::Wyoming
    } else {
        Error::Control
    })
}

async fn screen(directory: &Path) -> Screen {
    let token = directory.join("pairing");
    std::fs::write(&token, "synthetic-screen-pair").unwrap();
    Screen::bind(Some(&ScreenConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        pairing_token_file: token,
    }))
    .await
    .unwrap()
}

async fn request(screen: &Screen, request: &str) -> BufReader<TcpStream> {
    let mut stream = TcpStream::connect(screen.address().unwrap()).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
        headers.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    assert!(headers.starts_with("HTTP/1.1 200 OK") || headers.starts_with("HTTP/1.1 100 Continue"));
    reader
}

// T3: actual loopback SSE, production error publication and release trace.
#[tokio::test]
async fn t3_uncancelled_terminal_error_reaches_screen_before_release() {
    for _ in 0..100 {
        let directory = tempfile::tempdir().unwrap();
        let mut screen = screen(directory.path()).await;
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        let mut stream = request(
            &screen,
            "GET /events?token=synthetic-screen-pair HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await;
        let health = Health::offline();
        View::new(&screen, &health, &mut trace)
            .failed(&Error::Unavailable)
            .await
            .unwrap();
        close_screen(&mut screen, &mut trace, true).await.unwrap();
        let mut tail = String::new();
        stream.read_to_string(&mut tail).await.unwrap();
        let frame = tail.lines().find_map(|line| line.strip_prefix("data: ")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(frame).unwrap(),
            json!({
                "seq":1, "state":{"state":"error", "message":"Conversation unavailable. Try another wake.", "relink":false}
            })
        );
        let rows: Vec<Value> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows[0]["event"], "bridge.error");
        assert_eq!(rows[1]["event"], "screen.released");
        assert_eq!(rows[1]["dropped_clients"], 0);
    }
}

// T3: 100 Continue proves this owned HTTP handler is awaiting a real body.
// Unsent body bytes cannot be drained; cancellation must bypass that wait.
#[tokio::test(start_paused = true)]
async fn t3_screen_release_bounds_stalled_client_and_cancellation_skips_drain() {
    for graceful in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let mut screen = screen(directory.path()).await;
        let path = directory.path().join("trace");
        let mut trace = Trace::open(&path, "api_key").unwrap();
        let mut stream = request(&screen, "POST /actions?token=synthetic-screen-pair HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\nExpect: 100-continue\r\n\r\n").await;
        let start = tokio::time::Instant::now();
        close_screen(&mut screen, &mut trace, graceful).await.unwrap();
        assert_eq!(
            start.elapsed(),
            std::time::Duration::from_millis(if graceful { 250 } else { 0 })
        );
        let row: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(row["event"], "screen.released");
        assert_eq!(row["dropped_clients"], usize::from(graceful));
        let mut tail = Vec::new();
        stream.read_to_end(&mut tail).await.unwrap();
        assert_eq!(tail, [] as [u8; 0]);
        assert!(screen.address().is_none());
    }
}
