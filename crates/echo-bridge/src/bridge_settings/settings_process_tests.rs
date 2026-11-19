//! T1 reader ownership and Linux T3 real exec with a private held stdin pipe.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

struct Reader {
    reads: Arc<AtomicUsize>,
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<std::io::Result<usize>>,
}

impl Read for Reader {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.entered.send(()).unwrap();
        self.release.recv().unwrap()
    }
}

// T1: multiple runtime generations subscribe to one in-memory reader. EOF,
// unexpected input and I/O failure retain their original clean/error meaning.
#[test]
fn settings_process_reader_survives_runtime_replacement_and_latches_result() {
    for result in [Ok(0), Ok(1), Err(std::io::ErrorKind::BrokenPipe.into())] {
        let clean = matches!(result, Ok(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let (entered, ready) = mpsc::channel();
        let (release, receive) = mpsc::channel();
        let input = Input::open(Reader {
            reads: reads.clone(),
            entered,
            release: receive,
        })
        .unwrap();
        ready.recv().unwrap();
        let first = runtime();
        first.block_on(async {
            drop(input.subscribe());
        });
        drop(first);
        let second = runtime();
        second.block_on(async {
            let waiting = input.subscribe();
            let also_waiting = input.subscribe();
            release.send(result).unwrap();
            assert_eq!(waiting.await.unwrap(), clean);
            assert_eq!(also_waiting.await.unwrap(), clean);
            assert_eq!(input.subscribe().await.unwrap(), clean);
        });
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

// T1: the final gate uses the same held reader after runtime destruction. EOF
// or malformed input received during cleanup cannot be lost with a subscriber.
#[test]
fn settings_process_final_gate_observes_cleanup_control_and_durable_kill() {
    for result in [Ok(0), Ok(1), Err(std::io::ErrorKind::BrokenPipe.into())] {
        let clean = matches!(result, Ok(0));
        let reads = Arc::new(AtomicUsize::new(0));
        let (entered, ready) = mpsc::channel();
        let (release, receive) = mpsc::channel();
        let input = Input::open(Reader {
            reads: reads.clone(),
            entered,
            release: receive,
        })
        .unwrap();
        ready.recv().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let kill = directory.path().join("bridge-kill");
        assert_eq!(input.replacement_gate(Some(&kill)), Ok(ReplacementGate::Ready));
        std::fs::write(&kill, b"").unwrap();
        assert_eq!(input.replacement_gate(Some(&kill)), Ok(ReplacementGate::Killed));
        std::fs::write(&kill, b"invalid").unwrap();
        assert_eq!(input.replacement_gate(Some(&kill)), Err(Error::Control));
        std::fs::remove_file(&kill).unwrap();
        assert_eq!(input.replacement_gate(Some(&kill)), Ok(ReplacementGate::Ready));
        let runtime = runtime();
        runtime.block_on(async {
            let stop = input.subscribe();
            release.send(result).unwrap();
            assert_eq!(stop.await.unwrap(), clean);
        });
        drop(runtime);
        // A later kill cannot turn an EOF decision into an exec attempt.
        std::fs::write(&kill, b"").unwrap();
        let expected = if clean {
            Ok(ReplacementGate::Shutdown)
        } else {
            Err(Error::Control)
        };
        assert_eq!(input.replacement_gate(Some(&kill)), expected);
        assert_eq!(input.replacement_gate(None), expected);
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn t3_settings_process_real_exec_keeps_pid_image_argv_and_stdin() {
    use std::io::BufReader;
    use std::process::Stdio;
    const CHILD: &str = "ECHO_SETTINGS_EXEC_WITNESS";
    if let Some(path) = std::env::var_os(CHILD) {
        exec_child(std::path::Path::new(&path)).await;
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "bridge_settings::settings_process::tests::t3_settings_process_real_exec_keeps_pid_image_argv_and_stdin",
            "--nocapture",
        ])
        .env(CHILD, directory.path().join("phase"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let before = witness(&mut output);
    let after = witness(&mut output);
    assert_eq!(before, after);
    assert_eq!(before["pid"], child.id());
    // The original pipe is still held through exec. The second generation
    // announces readiness before we close it; EOF must give clean shutdown.
    drop(child.stdin.take());
    let ended = witness(&mut output);
    assert_eq!(ended, serde_json::json!({"eof": true}));
    assert!(child.wait().unwrap().success());
}

#[cfg(target_os = "linux")]
async fn exec_child(path: &std::path::Path) {
    let shutdown = shutdown().unwrap();
    let value = serde_json::json!({
        "pid": std::process::id(),
        "exe": std::fs::read_link("/proc/self/exe").unwrap(),
        "argv": std::fs::read("/proc/self/cmdline").unwrap(),
    });
    if !path.exists() {
        std::fs::write(path, b"exec requested").unwrap();
        emit(&value);
        let result = ProcessImage::capture().replace();
        panic!("exec unexpectedly returned: {result:?}");
    }
    emit(&value);
    emit(&serde_json::json!({"eof": shutdown.await.unwrap()}));
}

#[cfg(target_os = "linux")]
fn emit(value: &serde_json::Value) {
    use std::io::Write;
    let mut output = std::io::stdout().lock();
    writeln!(output, "EXEC_WITNESS {value}").unwrap();
    output.flush().unwrap();
}

#[cfg(target_os = "linux")]
fn witness(output: &mut impl std::io::BufRead) -> serde_json::Value {
    loop {
        let mut line = String::new();
        assert_ne!(output.read_line(&mut line).unwrap(), 0, "child ended before witness");
        if let Some((_, value)) = line.split_once("EXEC_WITNESS ") {
            return serde_json::from_str(value).unwrap();
        }
    }
}

// T3: a real exec syscall fails before replacing the image. The same reader
// and subscriptions remain usable, with no hidden retry or second read.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn t3_settings_process_exec_failure_preserves_control() {
    use std::os::unix::process::CommandExt;
    let reads = Arc::new(AtomicUsize::new(0));
    let (entered, ready) = mpsc::channel();
    let (release, receive) = mpsc::channel();
    let input = Input::open(Reader {
        reads: reads.clone(),
        entered,
        release: receive,
    })
    .unwrap();
    ready.recv().unwrap();
    let old = input.subscribe();
    let directory = tempfile::tempdir().unwrap();
    let error = ProcessImage::capture()
        .command()
        .unwrap()
        .current_dir(directory.path().join("absent"))
        .exec();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    let recovered = input.subscribe();
    release.send(Ok(0)).unwrap();
    assert!(old.await.unwrap());
    assert!(recovered.await.unwrap());
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}
