//! Port CLI source pin: portos-monolith a4bc375, cli/internal/cli/routes.go:202,
//! message_commands.go:204-290, `session_commands.go`; commands/session.go,
//! play_audio.go:129-145. Coordinator response 2 (2026-11-15) authorizes these
//! source-faithful forms. Playback completion is gated by returned timestamps.
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

/// Shared synchronous cleanup, including Drop after Tokio runtime teardown.
pub(super) const CLEANUP: Duration = Duration::from_secs(5);

pub(super) trait ReapChild {
    fn exited(&mut self) -> std::io::Result<Option<std::process::ExitStatus>>;
    fn force(&mut self) -> std::io::Result<()>;
}

impl ReapChild for Child {
    fn exited(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.try_wait()
    }
    fn force(&mut self) -> std::io::Result<()> {
        self.kill()
    }
}

#[allow(clippy::disallowed_methods, reason = "Existing host time")]
pub(super) fn stop_child(
    child: &mut impl ReapChild,
    remaining: Duration,
    forced: bool,
) -> Result<(std::process::ExitStatus, bool), String> {
    let start = Instant::now();
    stop_with(
        child,
        remaining,
        forced,
        || start.elapsed(),
        || {
            // Poll an OS exit, never infer completion from elapsed time or PID loss.
            std::thread::park_timeout(Duration::from_millis(1));
        },
    )
}

fn stop_with(
    child: &mut impl ReapChild,
    deadline: Duration,
    mut forced: bool,
    mut elapsed: impl FnMut() -> Duration,
    mut pending: impl FnMut(),
) -> Result<(std::process::ExitStatus, bool), String> {
    loop {
        if let Some(status) = child.exited().map_err(|_| "cleanup: cannot reap owned child")? {
            return Ok((status, forced));
        }
        if !forced && elapsed() >= deadline {
            child.force().map_err(|_| "cleanup: cannot kill owned child")?;
            forced = true;
        }
        pending();
    }
}

#[allow(clippy::disallowed_methods, reason = "Existing host time")]
fn reap(child: &mut Child, deadline: Instant, cancel: Option<&Path>) -> Result<bool, String> {
    loop {
        if cancelled(cancel) || Instant::now() >= deadline {
            let message = if cancelled(cancel) {
                "run cancelled"
            } else {
                "child deadline exceeded"
            };
            drop(child.stdin.take());
            return match stop_child(child, CLEANUP, false) {
                Ok(_) => Err(format!("{message}; child reaped")),
                Err(cleanup) => Err(format!("{message}; {cleanup}")),
            };
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) => std::thread::park_timeout(Duration::from_millis(5)),
            Err(_) => {
                drop(child.stdin.take());
                let primary = "child deadline or wait failed; child output discarded";
                return match stop_child(child, CLEANUP, false) {
                    Ok(_) => Err(primary.into()),
                    Err(cleanup) => Err(format!("{primary}; {cleanup}")),
                };
            }
        }
    }
}

#[allow(clippy::disallowed_methods, reason = "Existing host time")]
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
    reaped: bool,
    #[cfg(test)]
    collected: Option<std::sync::mpsc::Sender<std::process::ExitStatus>>,
}

impl Pending {
    #[allow(clippy::disallowed_methods, reason = "Existing host time")]
    pub fn start(executable: &Path, args: &[String], deadline: Instant, cancel: Option<&Path>) -> Result<Self, String> {
        if cancelled(cancel) {
            return Err("run cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("child deadline exceeded before launch".into());
        }
        let child = Command::new(executable)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "cannot launch selected tool; child output discarded")?;
        // Install the owner before any fallible pipe/reader setup.
        let mut pending = Self {
            child,
            reader: None,
            deadline,
            cancel: cancel.map(Path::to_path_buf),
            reaped: false,
            #[cfg(test)]
            collected: None,
        };
        let stdout = pending.child.stdout.take().ok_or("cannot capture child stdout")?;
        pending.reader = Some(
            std::thread::Builder::new()
                .spawn(move || {
                    let mut bytes = Vec::new();
                    // Continue draining after the cap to avoid deadlocking the child.
                    let mut stream = stdout;
                    stream.by_ref().take(LIMIT + 1).read_to_end(&mut bytes)?;
                    std::io::copy(&mut stream, &mut std::io::sink())?;
                    Ok::<_, std::io::Error>(bytes)
                })
                .map_err(|_| "child reader failed")?,
        );
        Ok(pending)
    }

    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        // Commands never consume input from the caller; normal completion sees
        // the same EOF as the former null stdin, while an abandoned Pending
        // still owns a closeable control pipe until Drop.
        drop(self.child.stdin.take());
        let status = reap(&mut self.child, self.deadline, self.cancel.as_deref());
        // try_wait caches the collected status; a wait failure must not claim reaping.
        self.reaped = self.child.try_wait().is_ok_and(|status| status.is_some());
        let output = self
            .reader
            .take()
            .ok_or("child reader missing".to_owned())
            .and_then(|reader| {
                reader
                    .join()
                    .map_err(|_| "child reader failed".to_owned())?
                    .map_err(|_| "child stdout read failed".to_owned())
            });
        match (status, output) {
            (Err(primary), Err(cleanup)) => Err(format!("{primary}; {cleanup}")),
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(success), Ok(output)) => {
                if !success || u64::try_from(output.len()).unwrap_or(u64::MAX) > LIMIT {
                    return Err("selected tool failed or exceeded output limit; child output discarded".into());
                }
                Ok(output)
            }
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.reaped {
            drop(self.child.stdin.take());
            self.reaped = stop_child(&mut self.child, CLEANUP, false).is_ok();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        #[cfg(test)]
        if let Some(observer) = self.collected.take()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            let _ = observer.send(status);
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
    #[allow(clippy::disallowed_methods, reason = "Host time for test")]
    fn expired_budget_fails_even_for_an_otherwise_successful_tool() {
        let result = controlled(Path::new("python"), &["-c".into(), "pass".into()], Instant::now(), None);
        assert_eq!(result, Err("child deadline exceeded before launch".into()));
    }

    // T3 real local child. The event never fires; the timeout is a ceiling,
    // not a synchronization delay. Returning proves kill/wait/pipe reaping.
    #[test]
    #[allow(clippy::disallowed_methods, reason = "Host time for test")]
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

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::cell::Cell;

    fn success() -> std::process::ExitStatus {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(0)
    }

    struct Fake<'a> {
        time: &'a Cell<u64>,
        ready_at: u64,
        kills: usize,
        waits: usize,
        fail_wait: bool,
    }

    impl ReapChild for Fake<'_> {
        fn exited(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            self.waits += 1;
            if self.fail_wait {
                return Err(std::io::Error::other("private wait failure"));
            }
            Ok((self.time.get() >= self.ready_at || self.kills == 1).then(success))
        }
        fn force(&mut self) -> std::io::Result<()> {
            assert_eq!(self.time.get(), 5000);
            self.kills += 1;
            Ok(())
        }
    }

    #[test]
    fn grace_collects_delayed_exit_or_forces_only_at_exact_deadline() {
        for ready_at in [4999, u64::MAX] {
            let time = Cell::new(0);
            let mut child = Fake {
                time: &time,
                ready_at,
                kills: 0,
                waits: 0,
                fail_wait: false,
            };
            let result = stop_with(
                &mut child,
                CLEANUP,
                false,
                || Duration::from_millis(time.get()),
                || {
                    time.set(time.get() + 1);
                },
            );
            assert_eq!(result, Ok((success(), ready_at == u64::MAX)));
            assert_eq!(child.kills, usize::from(ready_at == u64::MAX));
            assert_eq!(child.waits, if ready_at == 4999 { 5000 } else { 5002 });
        }
    }

    #[test]
    fn collected_exit_never_kills_and_wait_failure_never_claims_reaping() {
        let time = Cell::new(0);
        let mut child = Fake {
            time: &time,
            ready_at: 0,
            kills: 0,
            waits: 0,
            fail_wait: false,
        };
        assert_eq!(
            stop_with(&mut child, Duration::ZERO, false, || Duration::ZERO, || {}),
            Ok((success(), false))
        );
        assert_eq!((child.kills, child.waits), (0, 1));
        child.fail_wait = true;
        assert_eq!(
            stop_with(&mut child, CLEANUP, false, || Duration::ZERO, || {}),
            Err("cleanup: cannot reap owned child".into())
        );
        assert_eq!((child.kills, child.waits), (0, 2));
    }

    // T3: actual local pipe/exit-status boundary; no runtime compilation or sleeps.
    #[test]
    #[allow(clippy::disallowed_methods, reason = "Host time for test")]
    fn t3_pending_drop_closes_stdin_collects_status_and_joins_reader() {
        let python = "python3";
        let mut pending = Pending::start(
            Path::new(python),
            &[
                "-c".into(),
                "import sys; print('ready', flush=True); sys.stdin.buffer.read(); sys.stdout.buffer.write(b'exit')"
                    .into(),
            ],
            Instant::now() + Duration::from_secs(20),
            None,
        )
        .unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        pending.collected = Some(send);
        drop(pending);
        assert_eq!(receive.try_recv().unwrap(), success());
    }

    #[test]
    fn t3_success_retains_exact_stdout_and_nonzero_stays_sanitized() {
        let python = Path::new("python3");
        assert_eq!(
            bytes(
                python,
                &[
                    "-c".into(),
                    "import sys; sys.stdout.buffer.write(b'exact\\x00bytes')".into()
                ]
            ),
            Ok(b"exact\0bytes".to_vec())
        );
        assert_eq!(
            bytes(python, &["-c".into(), "print('private'); raise SystemExit(7)".into()]),
            Err("selected tool failed or exceeded output limit; child output discarded".into())
        );
    }
}
