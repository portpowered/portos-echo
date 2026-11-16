use echo_contracts::wyoming::{Decoder, Event, Header, encode};
use serde_json::Value;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdin, ChildStdout};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct Session(pub PathBuf);

impl Session {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "lab sim bridge-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // The name carries this process id; one that already exists is a stale leftover of an
        // interrupted earlier process that had the same id (Windows reuses ids), never a live one.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_echo-lab-sim-bridge"));
        command
            .env_remove("ECHO_LAB_WYOMING_ADDR")
            .env("ECHO_LAB_TRACE_PATH", self.0.join("trace.jsonl"));
        command
    }

    pub fn start(&self, addr: SocketAddr) -> Bridge {
        let mut command = self.command();
        command.env("ECHO_LAB_WYOMING_ADDR", addr.to_string());
        let mut child = tokio::process::Command::from(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Bridge {
            input: child.stdin.take(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
        }
    }

    pub fn trace(&self) -> Vec<Value> {
        std::fs::read_to_string(self.0.join("trace.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct Bridge {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Bridge {
    pub async fn line(&mut self) -> Value {
        let mut line = String::new();
        let size = tokio::time::timeout(Duration::from_secs(5), self.output.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(size, 0, "bridge exited before status record");
        serde_json::from_str(&line).unwrap()
    }

    pub async fn finish(mut self, code: i32) -> String {
        if code == 0 {
            drop(self.input.take());
        }
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        let mut errors = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut errors)
            .await
            .unwrap();
        assert_eq!(status.code(), Some(code), "{errors}");
        errors
    }
}

pub async fn send(peer: &mut TcpStream, kind: &str, data: Value, payload: Vec<u8>) {
    let event = Event {
        header: Header {
            r#type: kind.into(),
            data: Some(data),
            data_length: 0,
            payload_length: 0,
        },
        data: None,
        payload,
    };
    peer.write_all(&encode(&event)).await.unwrap();
}

pub async fn receive(peer: &mut TcpStream, decoder: &mut Decoder) -> Event {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let byte = peer.read_u8().await.unwrap();
            if let Some(event) = decoder.feed(&[byte]).unwrap().pop() {
                return event;
            }
        }
    })
    .await
    .unwrap()
}
