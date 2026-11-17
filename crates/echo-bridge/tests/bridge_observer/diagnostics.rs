//! Bounded, failure-only observations; never read the child's stdout here.
use super::*;
use std::fmt::Write as _;
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
};

const TAIL: usize = 8192;
type Hosts = Mutex<HashMap<u16, Weak<Capture>>>;
static HOSTS: OnceLock<Hosts> = OnceLock::new();

#[derive(Default)]
pub(super) struct Capture {
    bytes: Mutex<Vec<u8>>,
    truncated: AtomicBool,
    observer: AtomicU16,
}

impl Capture {
    pub(super) fn append(&self, bytes: &[u8]) {
        let mut tail = self.bytes.lock().unwrap();
        tail.extend_from_slice(bytes);
        let excess = tail.len().saturating_sub(TAIL);
        tail.drain(..excess);
        if excess != 0 {
            self.truncated.store(true, Ordering::Relaxed);
        }
        for line in tail.split(|b| *b == b'\n') {
            if let Some(port) = line
                .strip_prefix(b"observer port: ")
                .and_then(|b| std::str::from_utf8(b).ok())
                .and_then(|s| s.trim().parse().ok())
            {
                self.observer.store(port, Ordering::Relaxed);
            }
        }
    }

    pub(super) fn raw(&self) -> String {
        String::from_utf8_lossy(&self.bytes.lock().unwrap()).into_owned()
    }
    pub(super) fn tail(&self) -> String {
        let tail = self.bytes.lock().unwrap();
        safe(complete_tail(&tail, self.truncated.load(Ordering::Relaxed)))
    }

    pub(super) fn register(self: &Arc<Self>, port: u16) {
        let mut hosts = HOSTS.get_or_init(Mutex::default).lock().unwrap();
        hosts.retain(|_, capture| capture.strong_count() != 0);
        hosts.insert(port, Arc::downgrade(self));
    }

    pub(super) fn host(port: u16) -> Option<Arc<Self>> {
        HOSTS.get_or_init(Mutex::default).lock().unwrap().get(&port)?.upgrade()
    }

    pub(super) async fn snapshot(&self) -> String {
        self.snapshot_before(Duration::from_secs(1)).await
    }
    pub(super) async fn snapshot_before(&self, ceiling: Duration) -> String {
        let port = self.observer.load(Ordering::Relaxed);
        if port == 0 {
            return "unavailable: observer port not reported".into();
        }
        let response = tokio::time::timeout(ceiling, async {
            let mut socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await?;
            let mut bytes = Vec::new();
            (&mut socket).take(32768).read_to_end(&mut bytes).await?;
            Ok::<_, std::io::Error>(bytes)
        })
        .await;
        match response {
            Ok(Ok(bytes)) => host_snapshot(&bytes),
            Ok(Err(_)) => "unavailable: observer connection failed".into(),
            Err(_) => "unavailable: observer deadline".into(),
        }
    }
}

fn host_snapshot(bytes: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return "unavailable: partial observer response".into();
    };
    let Some(log) = value["log"].as_str() else {
        return "unavailable: controller reading host log".into();
    };
    let state = if value["process"].is_null() {
        "alive".into()
    } else if let Some(code) = value["process"].as_i64() {
        format!("exited ({code})")
    } else {
        "absent".into()
    };
    format!("process: {state}\nlog tail:\n{}", safe(log.as_bytes()))
}

pub(super) fn safe(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "<empty>".into();
    }
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|line| {
            // This authored trace enum is a mode, never a credential. Keep the
            // startup event visible in the synthetic API-key test topology.
            let lower = line
                .to_ascii_lowercase()
                .replace("\"provider_auth_mode\":\"api_key\"", "");
            if [
                "bearer", "token", "secret", "private", "password", "api_key", "api-key", "account", "/", "\\",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
            {
                "<redacted>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn file_tail(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return "<unavailable>".into();
    };
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return "<missing>".into(),
        Err(_) => return "<unavailable>".into(),
    };
    let result = (|| {
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(length.saturating_sub(TAIL as u64)))?;
        let mut bytes = Vec::new();
        std::io::Read::take(file, TAIL as u64).read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(safe(complete_tail(&bytes, length > TAIL as u64)))
    })();
    result.unwrap_or_else(|_| "<unavailable>".into())
}

pub(super) async fn snapshot(process: &mut Process) -> String {
    let state = match process.child.try_wait() {
        Ok(None) => "alive".into(),
        Ok(Some(status)) => format!("exited ({:?})", status.code()),
        Err(_) => "unknown".into(),
    };
    let mut output = format!(
        "process: {state}\nstderr tail:\n{}\nbridge trace tail:\n{}",
        process.capture.tail(),
        file_tail(process.trace.as_deref())
    );
    if let Some(port) = process.port {
        let host = if let Some(host) = &process.host_capture {
            host.snapshot().await
        } else {
            "unavailable: controlled peer or missing controller".into()
        };
        // A former port is not an ownership handle. Never dial whoever may
        // have bound it after the owned host exited.
        write!(output, "\npinned host port reported: {port}\npinned host snapshot:\n{host}\nTCP observer action: skipped (port does not establish ownership)").unwrap();
    } else {
        output.push_str("\npinned host port reported: unavailable");
    }
    output
}

pub(super) fn drain(mut stderr: tokio::process::ChildStderr) -> (Arc<Capture>, tokio::task::JoinHandle<()>) {
    let capture = Arc::new(Capture::default());
    let drain = capture.clone();
    let task = tokio::spawn(async move {
        let mut buffer = [0; 4096];
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(count) => drain.append(&buffer[..count]),
            }
        }
    });
    (capture, task)
}

fn complete_tail(bytes: &[u8], truncated: bool) -> &[u8] {
    if truncated {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[], |start| &bytes[start + 1..])
    } else {
        bytes
    }
}
