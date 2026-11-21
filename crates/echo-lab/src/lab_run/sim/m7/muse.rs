//! Own generated trust, real daemon and private callback helper until reaped.
use super::super::process::Process;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Muse {
    pub daemon: Option<Process>,
    helper: Option<Process>,
    pub root: PathBuf,
    pub url: String,
    pub calls: Vec<Value>,
    pub reports: Vec<Value>,
    pub admission: Value,
}

pub fn prebuilt() -> Result<PathBuf, String> {
    let directory = std::env::current_exe()
        .map_err(|_| "m7: cannot locate artifacts")?
        .parent()
        .ok_or("m7: missing artifact directory")?
        .to_owned();
    for name in ["echo-muse-daemon", "echo-muse-report", "echo-muse-fake-gadget"] {
        if !directory.join(name).is_file() {
            return Err(format!(
                "m7: missing prebuilt {name}; run cargo build --locked -p echo-bridge -p echo-muse-bridge -p echo-lab --bins"
            ));
        }
    }
    Ok(directory)
}

impl Muse {
    pub async fn start(&mut self, python: &str, root: &Path, binaries: &Path) -> Result<(), String> {
        self.root = root.into();
        std::fs::create_dir(root).map_err(|_| "m7: cannot own Muse directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let private = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700));
            private.map_err(|_| "m7: private directory permissions")?;
        }
        self.helper = Some(Process::start(
            tokio::process::Command::new(python)
                .arg("-c")
                .arg(include_str!("fixture.py"))
                .arg(root)
                .arg(binaries),
        )?);
        self.helper
            .as_mut()
            .ok_or("m7: missing helper")?
            .expect("m7.prepared")
            .await?;
        self.daemon = Some(Process::start(
            tokio::process::Command::new(binaries.join("echo-muse-daemon"))
                .args(["--config"])
                .arg(root.join("daemon.json"))
                .arg("--control-stdin"),
        )?);
        let ready = self.daemon.as_mut().ok_or("m7: missing daemon")?.line().await?;
        if ready["ready"] != true {
            return Err("m7: daemon did not become ready".into());
        }
        let address: std::net::SocketAddr = ready["listen"]
            .as_str()
            .ok_or("m7: missing address")?
            .parse()
            .map_err(|_| "m7: invalid address")?;
        if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
            return Err("m7: daemon must bind loopback".into());
        }
        let origin = "https://127.0.0.1";
        self.url = format!("{origin}:{}", address.port());
        Ok(())
    }

    pub async fn terminal(&mut self, id: &str, negative: bool) -> Result<(), String> {
        let raw = std::fs::read(self.root.join("tasks.json")).map_err(|_| "m7: missing durable task")?;
        let snapshot: Value = serde_json::from_slice(&raw).map_err(|_| "m7: invalid durable task")?;
        let tasks = snapshot["tasks"]
            .as_array()
            .filter(|tasks| tasks.len() == 1)
            .ok_or("m7: expected one durable task")?;
        let record = &tasks[0];
        if record["task_id"] != id || record["state"] != "live" {
            return Err("m7: progress must belong to the current live task".into());
        }
        let deadline = record["deadline_unix_seconds"]
            .as_u64()
            .ok_or("m7: missing task deadline")?;
        self.admission = record.clone();
        if negative {
            let daemon = self.daemon.as_mut().ok_or("m7: missing daemon")?;
            daemon
                .send(&json!({"operation":"set_time","unix_seconds":deadline}))
                .await?;
            if daemon.line().await? != json!({"clock_unix_seconds":deadline}) {
                return Err("m7: deadline clock acknowledgement mismatch".into());
            }
        } else {
            let report = self.report(id).await?;
            self.reports.push(report);
        }
        Ok(())
    }

    pub async fn report(&mut self, id: &str) -> Result<Value, String> {
        let helper = self.helper.as_mut().ok_or("m7: missing helper")?;
        helper.send(&json!({"task_id":id})).await?;
        helper.expect("m7.report").await
    }

    pub fn collect(&mut self) -> Result<(), String> {
        let raw = std::fs::read_to_string(self.root.join("calls.jsonl")).map_err(|_| "m7: missing gadget calls")?;
        self.calls = raw
            .lines()
            .map(|line| serde_json::from_str(line).map_err(|_| "m7: invalid gadget call".into()))
            .collect::<Result<_, String>>()?;
        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let mut failures = vec![];
        if self.daemon.is_none() && !self.url.is_empty() {
            failures.push("m7: missing daemon owner during cleanup".into());
        } else {
            self.url.clear();
        }
        for process in [&mut self.daemon, &mut self.helper] {
            if let Some(mut child) = process.take()
                && let Err(error) = child.close().await
            {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}
