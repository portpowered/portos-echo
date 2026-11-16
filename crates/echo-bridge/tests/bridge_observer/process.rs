//! Single stdout owner; stderr is drained independently before any wait.
use super::*;
use tokio::process::{Child, ChildStdin, ChildStdout};

#[path = "diagnostics.rs"]
mod diagnostics;

pub(super) struct Process {
    name: &'static str,
    expected: String,
    pub(super) child: Child,
    capture: std::sync::Arc<diagnostics::Capture>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
    trace: Option<PathBuf>,
    port: Option<u16>,
    host_capture: Option<std::sync::Arc<diagnostics::Capture>>,
    pub(super) input: Option<ChildStdin>,
    pub(super) lines: Lines<BufReader<ChildStdout>>,
}

impl Process {
    pub(super) fn spawn(command: &mut Command) -> Self {
        let environment: std::collections::HashMap<_, _> = command
            .as_std()
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
            .collect();
        let trace = environment
            .get(std::ffi::OsStr::new("ECHO_LAB_TRACE_PATH"))
            .map(PathBuf::from);
        let port = environment
            .get(std::ffi::OsStr::new("ECHO_LAB_WYOMING_ADDR"))
            .and_then(|address| address.to_str())
            .and_then(|address| address.rsplit(':').next())
            .and_then(|port| port.parse().ok());
        let host_capture = port.and_then(diagnostics::Capture::host);
        let name = if Path::new(command.as_std().get_program())
            .file_stem()
            .is_some_and(|name| name == "echo-bridge")
        {
            "echo-bridge"
        } else {
            "host controller"
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(name == "echo-bridge")
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let (capture, stderr_task) = diagnostics::drain(child.stderr.take().unwrap());
        let expected = if name == "echo-bridge" {
            "bridge.ready"
        } else {
            "host port"
        }
        .into();
        Self {
            name,
            expected,
            child,
            capture,
            stderr_task: Some(stderr_task),
            trace,
            port,
            host_capture,
            input,
            lines,
        }
    }
    pub(super) fn with_host(mut self, port: u16, trace: PathBuf) -> Self {
        self.port = Some(port);
        self.host_capture = diagnostics::Capture::host(port);
        self.trace = Some(trace);
        self
    }
    pub(super) async fn line(&mut self) -> String {
        let result = self.observe(OBSERVATION_TIMEOUT).await;
        assert!(result.is_ok(), "{}", result.as_ref().err().unwrap());
        result.unwrap()
    }
    async fn observe(&mut self, deadline: Duration) -> Result<String, String> {
        let response = tokio::time::timeout(deadline, self.lines.next_line()).await;
        let failure = match response {
            Ok(Ok(Some(line))) => {
                if self.name == "host controller"
                    && self.expected == "host port"
                    && let Ok(port) = line.trim().parse()
                {
                    self.capture.register(port);
                }
                if self.name == "echo-bridge" {
                    self.expected = "bridge.run_completed".into();
                }
                return Ok(line);
            }
            Err(_) => "timed out",
            Ok(Ok(None)) => "EOF before response",
            Ok(Err(_)) => "stdout read failed",
        };
        let snapshot = diagnostics::snapshot(self).await;
        let failure = if failure == "timed out" {
            format!("timed out after {deadline:?}")
        } else {
            failure.into()
        };
        Err(format!(
            "{} pid {:?}: {} waiting for {}\n{}",
            self.name,
            self.child.id(),
            failure,
            self.expected,
            snapshot
        ))
    }
    pub(super) async fn command(&mut self, command: &str) -> String {
        self.expected = format!("host command reply: {command}");
        self.input
            .as_mut()
            .unwrap()
            .write_all(format!("{command}\n").as_bytes())
            .await
            .unwrap();
        self.input.as_mut().unwrap().flush().await.unwrap();
        self.line().await
    }
    pub(super) async fn finish(&mut self, code: i32) {
        drop(self.input.take());
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        let error = self.stderr().await;
        assert_eq!(status.code(), Some(code), "{}", diagnostics::safe(error.as_bytes()));
    }
    pub(super) async fn stderr(&mut self) -> String {
        if let Some(mut task) = self.stderr_task.take() {
            let drained = tokio::time::timeout(Duration::from_secs(1), &mut task).await;
            if drained.is_err() {
                task.abort();
            }
            drained.unwrap().unwrap();
        }
        self.capture.raw()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}

#[path = "observer_tests.rs"]
mod observer_tests;

#[path = "startup_tests.rs"]
mod startup_tests;
