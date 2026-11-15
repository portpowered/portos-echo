//! Port CLI source pin: portos-monolith a4bc375, cli/internal/cli/routes.go:202,
//! message_commands.go:204-290, `session_commands.go`; commands/session.go,
//! play_audio.go:129-145. Coordinator response 2 (2026-11-15) authorizes these
//! source-faithful forms. Listening chirps also need --output per `check()`.
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const LIMIT: u64 = 1024 * 1024;
const TABLE: &[(&str, &[&str])] = &[
    ("hold", &["session", "hold", "{ring}", "--camera", "--listen", "--json"]),
    ("release", &["session", "release", "{hold}", "--json"]),
    ("status", &["session", "status", "{hold}", "--json"]),
    (
        "snapshot",
        &[
            "message", "snapshot", "{ring}", "--lease", "{lease}", "--output", "{output}", "--json",
        ],
    ),
    (
        "record",
        &[
            "message",
            "record",
            "{ring}",
            "--lease",
            "{lease}",
            "--audio-only",
            "--duration",
            "10",
            "--output",
            "{output}",
            "--json",
        ],
    ),
    (
        "chirp",
        &[
            "message",
            "play-audio",
            "{ring}",
            "--lease",
            "{lease}",
            "--chirp",
            "--listen",
            "3",
            "--output",
            "{output}",
            "--json",
        ],
    ),
    (
        "file",
        &[
            "message",
            "play-audio",
            "{ring}",
            "--lease",
            "{lease}",
            "--file",
            "{file}",
            "--listen",
            "10",
            "--output",
            "{output}",
            "--json",
        ],
    ),
    (
        "text",
        &[
            "message",
            "play-audio",
            "{ring}",
            "--lease",
            "{lease}",
            "--text",
            "{text}",
            "--speech-endpoint",
            "{endpoint}",
            "--speech-model",
            "{model}",
            "--listen",
            "10",
            "--output",
            "{output}",
            "--json",
        ],
    ),
];

pub fn argv(operation: &str, values: &[(&str, &str)]) -> Result<Vec<String>, String> {
    if let Some(base) = operation.strip_suffix("_only") {
        let mut args = argv(base, values)?;
        if let Some(index) = args.iter().position(|arg| arg == "--listen") {
            args.drain(index..index + 4);
        }
        return Ok(args);
    }
    let (_, template) = TABLE
        .iter()
        .find(|(name, _)| *name == operation)
        .ok_or("unknown port operation")?;
    template
        .iter()
        .map(|arg| {
            if arg.starts_with('{') {
                values
                    .iter()
                    .find(|(key, _)| key == arg)
                    .map(|(_, value)| (*value).to_owned())
                    .ok_or_else(|| "missing port argument".into())
            } else {
                Ok((*arg).to_owned())
            }
        })
        .collect()
}

fn reap(child: &mut Child, deadline: Instant, cancel: Option<&Path>) -> Result<bool, String> {
    loop {
        if cancelled(cancel) || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if cancelled(cancel) {
                "run cancelled; child reaped"
            } else {
                "child deadline exceeded; child reaped"
            }
            .into());
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("child deadline or wait failed; child output discarded".into());
            }
        }
    }
}

pub fn bytes(executable: &Path, args: &[String]) -> Result<Vec<u8>, String> {
    controlled(executable, args, Instant::now() + Duration::from_secs(30), None)
}

pub fn cancelled(cancel: Option<&Path>) -> bool {
    cancel.is_some_and(Path::exists)
}

pub fn controlled(
    executable: &Path,
    args: &[String],
    deadline: Instant,
    cancel: Option<&Path>,
) -> Result<Vec<u8>, String> {
    Pending::start(executable, args, deadline, cancel)?.finish()
}

pub struct Pending {
    child: Child,
    reader: Option<std::thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    deadline: Instant,
    cancel: Option<std::path::PathBuf>,
}

impl Pending {
    pub fn start(executable: &Path, args: &[String], deadline: Instant, cancel: Option<&Path>) -> Result<Self, String> {
        if cancelled(cancel) {
            return Err("run cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("child deadline exceeded before launch".into());
        }
        let mut child = Command::new(executable)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "cannot launch selected tool; child output discarded")?;
        let stdout = child.stdout.take().ok_or("cannot capture child stdout")?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            // Continue draining after the cap to avoid deadlocking the child.
            let mut stream = stdout;
            stream.by_ref().take(LIMIT + 1).read_to_end(&mut bytes)?;
            std::io::copy(&mut stream, &mut std::io::sink())?;
            Ok::<_, std::io::Error>(bytes)
        });
        Ok(Self {
            child,
            reader: Some(reader),
            deadline,
            cancel: cancel.map(Path::to_path_buf),
        })
    }

    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        let status = reap(&mut self.child, self.deadline, self.cancel.as_deref());
        let output = self
            .reader
            .take()
            .ok_or("child reader missing")?
            .join()
            .map_err(|_| "child reader failed")?
            .map_err(|_| "child stdout read failed")?;
        if !status? || u64::try_from(output.len()).unwrap_or(u64::MAX) > LIMIT {
            return Err("selected tool failed or exceeded output limit; child output discarded".into());
        }
        Ok(output)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

pub fn json(executable: &Path, args: &[String]) -> Result<Value, String> {
    serde_json::from_slice(&bytes(executable, args)?)
        .map_err(|_| "selected tool returned malformed JSON; child output discarded".into())
}

pub fn helper(python: &str, operation: &str, args: &[String]) -> Result<Value, String> {
    let mut command_args = vec!["-c".into(), HOST.into(), operation.into()];
    command_args.extend_from_slice(args);
    json(Path::new(python), &command_args)
}

pub const HOST: &str = concat!(include_str!("run_pixels.py"), "\n", include_str!("run_host.py"));

#[cfg(test)]
mod tests {
    use super::*;

    // An expired budget must refuse launching even an otherwise successful tool.
    #[test]
    fn expired_budget_fails_even_for_an_otherwise_successful_tool() {
        let result = controlled(Path::new("python"), &["-c".into(), "pass".into()], Instant::now(), None);
        assert_eq!(result, Err("child deadline exceeded before launch".into()));
    }

    // T3 real local child. The event never fires; the timeout is a ceiling,
    // not a synchronization delay. Returning proves kill/wait/pipe reaping.
    #[test]
    fn stalled_child_is_reaped_at_its_operation_deadline() {
        let result = controlled(
            Path::new("python"),
            &["-c".into(), "import threading; threading.Event().wait()".into()],
            Instant::now() + Duration::from_millis(100),
            None,
        );
        assert_eq!(result, Err("child deadline exceeded; child reaped".into()));
    }
}
