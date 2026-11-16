use std::path::Path;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub struct Process {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}

impl Process {
    pub fn start(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "launch: cannot spawn owned process")?;
        let input = child.stdin.take();
        let output = child.stdout.take().ok_or("launch: missing owned stdout")?;
        Ok(Self { child, input, output })
    }

    pub async fn line(&mut self) -> Result<Value, String> {
        let mut bytes = Vec::new();
        loop {
            let byte = self
                .output
                .read_u8()
                .await
                .map_err(|_| "process: exited before observation")?;
            if bytes.len() >= 4096 {
                return Err("process: oversized observation".into());
            }
            if byte == b'\n' {
                return serde_json::from_slice(&bytes).map_err(|_| "process: malformed observation".into());
            }
            bytes.push(byte);
        }
    }

    pub async fn expect(&mut self, event: &str) -> Result<Value, String> {
        let value = self.line().await?;
        if value["event"] != event {
            return Err("process: unexpected observation".into());
        }
        Ok(value)
    }

    pub async fn send(&mut self, value: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(value).map_err(|_| "process: cannot encode control")?;
        bytes.push(b'\n');
        let input = self.input.as_mut().ok_or("process: control closed")?;
        input
            .write_all(&bytes)
            .await
            .map_err(|_| "process: cannot write control")?;
        input.flush().await.map_err(|_| "process: cannot flush control".into())
    }

    pub async fn close(&mut self) -> Result<(), String> {
        drop(self.input.take());
        let status = self
            .child
            .wait()
            .await
            .map_err(|_| "cleanup: cannot reap owned child")?;
        if !status.success() {
            return Err("cleanup: owned child exited nonzero".into());
        }
        Ok(())
    }

    pub async fn terminate(&mut self) {
        drop(self.input.take());
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    pub fn exited(&mut self) -> Result<bool, String> {
        self.child
            .try_wait()
            .map(|s| s.is_some())
            .map_err(|_| "process: cannot observe child exit".into())
    }
}

pub fn host(python: &str) -> Command {
    let mut command = Command::new(python);
    command
        .arg("-u")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lab_run/sim/host.py"));
    command
}
