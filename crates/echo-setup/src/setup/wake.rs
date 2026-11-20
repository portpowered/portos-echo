//! Fixed shell observations only. Neither the cursor nor log bytes are public.
use crate::{Failure, Result, adb::Adb, cli::Options};
use std::time::Duration;
use tokio::time::Instant;

/// A fresh confirmed detection and a subsequent hassmic link observation.
/// These do not establish a Wyoming run or successful provider audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WakeEvidence {
    pub detected: bool,
    pub linked: bool,
}

/// Capture immediately before asking the user to say Computer. This owns no
/// remote mutation; cancellation kills only its local adb child process.
pub struct WakeAttempt {
    adb: Adb,
    cursor: String,
    deadline: Instant,
}

impl WakeAttempt {
    /// # Errors
    /// Fixed prerequisite, deadline or unsafe log observation categories.
    pub async fn begin(options: &Options, deadline: Instant) -> Result<Self> {
        bounded(deadline, async {
            let mut adb = Adb::discover(options.adb.as_deref())?;
            adb.select(options.serial.as_deref()).await?;
            let (success, cursor) = adb.script("wake-cursor").await?;
            if !success || !valid_cursor(&cursor) {
                return Err(Failure("setup_wake_unavailable"));
            }
            Ok(Self {
                adb,
                cursor: cursor.trim_end_matches('\n').to_owned(),
                deadline,
            })
        })
        .await
    }

    /// The authored shell rejects replacement, truncation and prefix rewrite.
    /// Never rebaseline silently after one of those failures.
    /// # Errors
    /// Fixed observation/deadline failure without log contents or cursor data.
    pub async fn observe(&self) -> Result<WakeEvidence> {
        bounded(self.deadline, async {
            // The cursor grammar admits only lowercase hex, digits and colons.
            let operation = format!("wake-check '{}'", self.cursor);
            let (success, output) = self.adb.script(&operation).await?;
            if !success {
                return Err(Failure("setup_wake_unavailable"));
            }
            parse_evidence(&output)
        })
        .await
    }

    /// Poll the read-only shell observer at a bounded rate. This returns log
    /// evidence only; the setup owner must independently establish a fresh run.
    /// # Errors
    /// Fixed observation/deadline failure; never a partial success on expiry.
    pub async fn wait_for_link(&self) -> Result<WakeEvidence> {
        bounded(self.deadline, async {
            loop {
                let evidence = self.observe().await?;
                if evidence.detected && evidence.linked {
                    return Ok(evidence);
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
    }
}

pub(super) async fn bounded<T>(deadline: Instant, operation: impl Future<Output = Result<T>>) -> Result<T> {
    if Instant::now() >= deadline {
        return Err(Failure("setup_timeout"));
    }
    tokio::time::timeout_at(deadline, operation)
        .await
        .map_err(|_| Failure("setup_timeout"))?
}

pub(super) fn valid_cursor(output: &str) -> bool {
    let Some(cursor) = output.strip_suffix('\n') else {
        return false;
    };
    let fields: Vec<_> = cursor.split(':').collect();
    fields.len() == 4
        && fields[..3].iter().all(|value| {
            !value.is_empty()
                && value.len() <= 15
                && (value.len() == 1 || !value.starts_with('0'))
                && value.bytes().all(|byte| byte.is_ascii_digit())
        })
        && fields[3].len() == 64
        && fields[3]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_evidence(output: &str) -> Result<WakeEvidence> {
    match output {
        "wake=pending\nlink=pending\n" => Ok(WakeEvidence {
            detected: false,
            linked: false,
        }),
        "wake=confirmed\nlink=pending\n" => Ok(WakeEvidence {
            detected: true,
            linked: false,
        }),
        "wake=confirmed\nlink=connected\n" => Ok(WakeEvidence {
            detected: true,
            linked: true,
        }),
        _ => Err(Failure("setup_wake_unavailable")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_grammar_is_exact_and_cannot_inject_remote_shell() {
        let digest = "a".repeat(64);
        assert!(valid_cursor(&format!("0:1:999999999999999:{digest}\n")));
        for prefix in [
            "",
            "1:2",    // lint-rules: allow-literal inert device-log cursor grammar fixture; no network destination
            "01:2:3", // lint-rules: allow-literal inert device-log cursor grammar fixture; no network destination
            "1:2:-3",
            "1:2:1000000000000000", // lint-rules: allow-literal inert device-log cursor grammar fixture; no network destination
            "1:2:3:4", // lint-rules: allow-literal inert device-log cursor grammar fixture; no network destination
            "1;id:2:3", // lint-rules: allow-literal inert device-log cursor grammar fixture; no network destination
        ] {
            assert!(!valid_cursor(&format!("{prefix}:{digest}\n")));
        }
        for suffix in [
            digest.to_uppercase(),
            "g".repeat(64),
            "a".repeat(63),
            format!("{digest}\n"),
        ] {
            assert!(!valid_cursor(&format!("1:2:3:{suffix}\n")));
        }
        assert!(!valid_cursor(&format!("1:2:3:{digest}")));
    }

    #[test]
    fn evidence_rejects_partial_duplicate_private_and_impossible_frames() {
        for output in [
            "wake=pending\nlink=connected\n",
            "wake=confirmed\nlink=connected",
            "link=connected\nwake=confirmed\n",
            "wake=confirmed\nwake=confirmed\nlink=connected\n",
            "wake=confirmed\nlink=connected\nSYNTHETIC-TOKEN\n",
        ] {
            assert_eq!(parse_evidence(output), Err(Failure("setup_wake_unavailable")));
        }
    }
}
