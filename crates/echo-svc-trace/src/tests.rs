use super::*;
use echo_core_api::PendingTrace;
use serde_json::{Value, json};
use std::collections::VecDeque;

#[derive(Default)]
struct Source(Mutex<VecDeque<PendingTrace>>);

impl TraceDrain for Source {
    fn next_row(&self) -> Option<PendingTrace> {
        self.0.lock().unwrap().pop_front()
    }
}

fn row(fields: Value) -> TraceRow {
    let Value::Object(fields) = fields else {
        panic!("expected object fields");
    };
    TraceRow {
        event: "run.started".into(),
        elapsed_ms: 41,
        mode: "api_key",
        run: Some("run-1".into()),
        fields,
        payload_fields: &[],
        limit: 4096,
    }
}

#[test]
fn oversized_record_leaves_prior_bytes_and_the_sink_usable() {
    let mut sink = Vec::new();
    let expected =
        b"{\"elapsed_ms\":41,\"event\":\"run.started\",\"provider_auth_mode\":\"api_key\",\"run_id\":\"run-1\"}\n";
    write_row(&mut sink, row(json!({}))).unwrap();
    assert_eq!(
        write_row(&mut sink, row(json!({"reason":"x".repeat(4096)}))),
        Err(TraceFailure)
    );
    assert_eq!(sink, expected);
    write_row(&mut sink, row(json!({}))).unwrap();
    assert_eq!(sink, expected.repeat(2));
}

struct GatedSink {
    written: Arc<Mutex<Vec<u8>>>,
    entered: SyncSender<()>,
    release: Receiver<()>,
}

impl Write for GatedSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.entered.send(()).unwrap();
        self.release.recv().unwrap();
        self.written.lock().unwrap().write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn stalled_writer_keeps_receipt_pending_and_drains_later_accepted_rows() {
    let source = Arc::new(Source::default());
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (entered, writing) = sync_channel(1);
    let (unblock, release) = sync_channel(2);
    let (receipt, committed) = sync_channel(1);
    let writer = Writer::start(
        GatedSink {
            written: bytes.clone(),
            entered,
            release,
        },
        source.clone(),
    )
    .unwrap();
    source.0.lock().unwrap().push_back(PendingTrace {
        row: row(json!({"n":1})),
        receipt: Some(receipt),
    });
    writer.wake();
    writing.recv().unwrap();
    assert!(matches!(
        committed.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    source.0.lock().unwrap().push_back(PendingTrace {
        row: row(json!({"n":2})),
        receipt: None,
    });
    unblock.send(()).unwrap();
    assert_eq!(committed.recv().unwrap(), Ok(()));
    writing.recv().unwrap();
    unblock.send(()).unwrap();
    writer.finish().unwrap();
    let output = bytes.lock().unwrap();
    let rows: Vec<Value> = output
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(
        rows.iter().map(|row| row["n"].clone()).collect::<Vec<_>>(),
        [json!(1), json!(2)]
    );
}
