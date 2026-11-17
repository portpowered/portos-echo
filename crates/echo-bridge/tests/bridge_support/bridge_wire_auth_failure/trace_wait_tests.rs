use super::*;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::ReadBuf;

#[test]
fn trace_wait_split_exact_records_and_safe_tail() {
    let mut trace = Trace::default();
    assert!(!trace.consume(
        b"{\"event\":\"reply.started.extra\"}\n{\"event\":\"reply.",
        "reply.started"
    ));
    assert!(!trace.consume(b"started\",\"secret\":\"synthetic-private\"}", "reply.started"));
    assert_eq!(trace.records, 1);
    assert!(trace.consume(
        b"\ninvalid synthetic-private\n{\"event\":\"mic.frames\",\"elapsed_ms\":7}\n",
        "reply.started"
    ));
    assert_eq!(trace.records, 4);
    assert_eq!(trace.partial, Vec::<u8>::new());
    for _ in 0..10 {
        trace.consume(b"{\"event\":\"mic.frames\",\"elapsed_ms\":7}\n", "missing");
    }
    assert_eq!(trace.tail.len(), 8);
    let message = trace.failure("reply.started", "timed out", "bridge try_wait=alive");
    assert!(message.contains("mic.frames elapsed_ms=Some(7)"));
    assert!(message.contains("bridge try_wait=alive"));
    assert!(!message.contains("synthetic-private"));
}

struct Growing {
    chunks: VecDeque<Vec<u8>>,
    reads: usize,
}

impl AsyncRead for Growing {
    fn poll_read(mut self: Pin<&mut Self>, _: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        self.reads += 1;
        if let Some(bytes) = self.chunks.pop_front() {
            buffer.put_slice(&bytes);
        }
        Poll::Ready(Ok(()))
    }
}

#[tokio::test(start_paused = true)]
async fn trace_wait_eof_yields_and_keeps_deadline() {
    let mut reader = Growing {
        chunks: VecDeque::new(),
        reads: 0,
    };
    let mut trace = Trace::default();
    let start = tokio::time::Instant::now();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(4),
            read_events(&mut reader, &mut trace, "reply.started")
        )
        .await
        .is_err()
    );
    assert_eq!(start.elapsed(), Duration::from_millis(4));
    assert_eq!(reader.reads, 1);
    assert!(
        tokio::time::timeout(
            OBSERVATION_TIMEOUT,
            read_events(&mut reader, &mut trace, "reply.started")
        )
        .await
        .is_err()
    );
    assert_eq!(start.elapsed(), OBSERVATION_TIMEOUT + Duration::from_millis(4));
    assert_eq!(trace.bytes, 0);
}

#[tokio::test(start_paused = true)]
async fn trace_wait_growth_consumes_each_record_once() {
    let history = b"{\"event\":\"mic.frames\"}\n".repeat(100);
    let last = b"{\"event\":\"reply.started\"}\n".to_vec();
    let bytes = history.len() + last.len();
    let mut reader = Growing {
        chunks: std::iter::once(history)
            .chain(std::iter::repeat_n(vec![], 32))
            .chain(std::iter::once(last))
            .collect(),
        reads: 0,
    };
    let mut trace = Trace::default();
    read_events(&mut reader, &mut trace, "reply.started").await.unwrap();
    assert_eq!(trace.records, 101);
    assert_eq!(trace.bytes, bytes);
    assert_eq!(reader.reads, 34);
}

#[test]
fn trace_wait_unknown_process_state_redacts_io_details() {
    let state = child_state(Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "synthetic-private",
    )));
    assert_eq!(state, "bridge try_wait=unknown (PermissionDenied)");
    assert_eq!(child_state(Ok(None)), "bridge try_wait=alive");
    assert_eq!(summary(None), "[invalid JSON record]");
    assert_eq!(
        summary(Some(&json!({"event":"synthetic-private"}))),
        "[redacted event] elapsed_ms=None"
    );
}

fn panic_message(error: tokio::task::JoinError) -> String {
    let payload = error.into_panic();
    payload.downcast_ref::<String>().unwrap().clone()
}

#[tokio::test]
async fn trace_wait_read_failure_explains_expected_event_and_state() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("synthetic-private");
    std::fs::write(&file, b"not a directory").unwrap();
    let path = directory.path().to_path_buf();
    let error = tokio::spawn(async move {
        observe(&path, "reply.started", || "bridge try_wait=unknown".into()).await;
    })
    .await
    .unwrap_err();
    let message = panic_message(error);
    assert!(message.contains("trace wait for reply.started: read failed"));
    assert!(message.contains("bridge try_wait=unknown"));
    assert!(!message.contains("synthetic-private"));
}

#[tokio::test(start_paused = true)]
async fn trace_wait_missing_file_reports_timeout_without_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic-private");
    let error = tokio::spawn(async move {
        observe(&path, "reply.started", || "bridge try_wait=alive".into()).await;
    })
    .await
    .unwrap_err();
    let message = panic_message(error);
    assert!(message.contains("timed out after 30s; bridge try_wait=alive"));
    assert!(message.contains("bytes=0 records=0 partial_bytes=0"));
    assert!(message.contains("sanitized trace tail: []"));
    assert!(!message.contains("synthetic-private"));
}

#[tokio::test]
#[ignore = "T3 requires local child process"]
async fn trace_wait_process_preserves_stdout_and_reports_live_then_exited() {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.args(["/D", "/Q", "/C", "echo synthetic-ready & set /p line= & exit /b 0"]);
        command
    } else {
        let mut command = Command::new("sh");
        command.args(["-c", "echo synthetic-ready; read line; exit 0"]);
        command
    };
    let mut bridge = Process::spawn(&mut command);
    assert_eq!(bridge.line().await.trim(), "synthetic-ready");
    assert_eq!(child_state(bridge.child.try_wait()), "bridge try_wait=alive");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace");
    std::fs::write(&path, b"{\"event\":\"reply.started\"}\n").unwrap();
    wait_event(&path, "reply.started", &mut bridge).await;
    bridge.finish(0).await;
    let state = child_state(bridge.child.try_wait());
    assert!(state.starts_with("bridge try_wait=exited ("));
    assert_eq!(bridge.child.try_wait().unwrap().unwrap().code(), Some(0));
    assert_eq!(bridge.lines.next_line().await.unwrap(), None);
}
