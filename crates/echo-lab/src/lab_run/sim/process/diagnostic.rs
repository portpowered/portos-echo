//! Drain stderr without publishing child-controlled text or retaining unbounded data.
use tokio::{io::AsyncReadExt, process::ChildStderr, task::JoinHandle, time::Instant};

pub(super) struct Diagnostic {
    task: Option<JoinHandle<&'static str>>,
    summary: &'static str,
}

impl Diagnostic {
    pub(super) fn start(stderr: Option<ChildStderr>) -> Self {
        Self {
            task: stderr.map(|mut stderr| {
                tokio::spawn(async move {
                    let mut retained = Vec::new();
                    let mut buffer = [0; 4096];
                    while let Ok(count) = stderr.read(&mut buffer).await {
                        if count == 0 {
                            break;
                        }
                        let keep = count.min(4096 - retained.len());
                        retained.extend_from_slice(&buffer[..keep]);
                    }
                    classify(&retained)
                })
            }),
            summary: "unavailable",
        }
    }

    pub(super) async fn finish(&mut self, deadline: Instant) {
        if let Some(mut task) = self.task.take() {
            self.summary = if let Ok(Ok(summary)) = tokio::time::timeout_at(deadline, &mut task).await {
                summary
            } else {
                task.abort();
                "unavailable"
            };
        }
    }

    pub(super) fn summary(&self) -> &'static str {
        self.summary
    }
}

impl Drop for Diagnostic {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

fn classify(bytes: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(bytes);
    for fixed in [
        "echo-bridge: control",
        "echo-bridge: wyoming",
        "echo-bridge: screen",
        "echo-bridge: deadline",
        "echo-bridge: unavailable",
        "echo-bridge: audio",
        "echo-bridge: trace",
        "host thread cleanup failed",
        "BrokenPipeError",
        "RuntimeError",
        "AssertionError",
    ] {
        if text
            .lines()
            .any(|line| line == fixed || line.starts_with(&format!("{fixed}:")))
        {
            return fixed;
        }
    }
    if bytes.is_empty() {
        "empty"
    } else {
        "unrecognized stderr"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_diagnostics_publish_only_fixed_categories() {
        assert_eq!(classify(b"echo-bridge: wyoming\n"), "echo-bridge: wyoming");
        assert_eq!(classify(b"RuntimeError: private-fixture-data\n"), "RuntimeError");
        assert_eq!(classify(b"private-fixture-data\n"), "unrecognized stderr");
        assert_eq!(
            classify(b"echo-bridge: wyoming private-fixture-data\n"),
            "unrecognized stderr"
        );
        assert_eq!(classify(b""), "empty");
    }
}
