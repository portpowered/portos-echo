//! Paired fresh log evidence, without exposing either log or cursor.
use super::wake::{bounded, valid_cursor};
use crate::{Failure, Result, adb::Adb, cli::Options};
use std::time::Duration;
use tokio::time::Instant;

/// All three observations must come from the same invocation's log baselines.
/// They establish detection, link and bridge run admission, never provider/audio success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmationEvidence {
    pub detected: bool,
    pub linked: bool,
    pub run_started: bool,
}

impl ConfirmationEvidence {
    #[must_use]
    pub fn complete(self) -> bool {
        self.detected && self.linked && self.run_started
    }
}

/// Capture immediately before the wake instruction, after the runtime starts.
/// Both cursors and every later read use the same selected device and deadline.
pub struct WakeConfirmation {
    adb: Adb,
    cursors: [String; 2],
    deadline: Instant,
}

impl WakeConfirmation {
    /// # Errors
    /// Fixed selection, timeout or unsafe/missing trace/log categories.
    pub async fn begin(options: &Options, deadline: Instant) -> Result<Self> {
        bounded(deadline, async {
            let mut adb = Adb::discover(options.adb.as_deref())?;
            adb.select(options.serial.as_deref()).await?;
            Self::selected(adb, deadline).await
        })
        .await
    }

    pub(super) async fn selected(adb: Adb, deadline: Instant) -> Result<Self> {
        bounded(deadline, async {
            let (success, output) = adb.script("confirm-cursor").await?;
            if !success {
                return Err(Failure("setup_wake_unavailable"));
            }
            Ok(Self {
                adb,
                cursors: cursors(&output)?,
                deadline,
            })
        })
        .await
    }

    /// Never rebaseline after log replacement, truncation or prefix rewrite.
    /// # Errors
    /// Fixed observation/deadline categories; never logs or identifiers.
    pub async fn observe(&self) -> Result<ConfirmationEvidence> {
        bounded(self.deadline, async {
            let operation = format!("confirm-check '{}' '{}'", self.cursors[0], self.cursors[1]);
            let (success, output) = self.adb.script(&operation).await?;
            if !success {
                return Err(Failure("setup_wake_unavailable"));
            }
            evidence(&output)
        })
        .await
    }

    /// # Errors
    /// Fixed observation failure or the absolute setup deadline; no partial success.
    pub async fn wait(&self) -> Result<ConfirmationEvidence> {
        bounded(self.deadline, async {
            loop {
                let observed = self.observe().await?;
                if observed.complete() {
                    return Ok(observed);
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
    }
}

fn cursors(output: &str) -> Result<[String; 2]> {
    let lines: Vec<_> = output.split_inclusive('\n').collect();
    if lines.len() != 2 || !lines.iter().all(|line| valid_cursor(line)) {
        return Err(Failure("setup_wake_unavailable"));
    }
    Ok([lines[0].trim_end().into(), lines[1].trim_end().into()])
}

fn evidence(output: &str) -> Result<ConfirmationEvidence> {
    let (wake, run) = output.rsplit_once("run=").ok_or(Failure("setup_wake_unavailable"))?;
    let (detected, linked) = match wake {
        "wake=pending\nlink=pending\n" => (false, false),
        "wake=confirmed\nlink=pending\n" => (true, false),
        "wake=confirmed\nlink=connected\n" => (true, true),
        _ => return Err(Failure("setup_wake_unavailable")),
    };
    let run_started = match run {
        "started\n" => true,
        "pending\n" => false,
        _ => return Err(Failure("setup_wake_unavailable")),
    };
    Ok(ConfirmationEvidence {
        detected,
        linked,
        run_started,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_requires_all_three_observations_and_exact_frames() -> Result<()> {
        for wake in [
            "pending\nlink=pending",
            "confirmed\nlink=pending",
            "confirmed\nlink=connected",
        ] {
            for run in ["pending", "started"] {
                let observed = evidence(&format!("wake={wake}\nrun={run}\n"))?;
                assert_eq!(observed.complete(), wake.ends_with("connected") && run == "started");
            }
        }
        for raw in [
            "wake=pending\nlink=connected\nrun=started\n",
            "wake=confirmed\nlink=connected\nrun=started",
            "wake=confirmed\nlink=connected\nrun=started\nSYNTHETIC-TOKEN\n",
            "wake=confirmed\nlink=connected\nrun=started\nrun=started\n",
        ] {
            assert_eq!(evidence(raw), Err(Failure("setup_wake_unavailable")));
        }
        let cursor = format!("1:2:3:{}\n", "a".repeat(64));
        assert!(cursors(&format!("{cursor}{cursor}")).is_ok());
        for raw in [
            cursor.clone(),
            format!("{cursor}{cursor}{cursor}"),
            format!("{cursor}';id\n"),
        ] {
            assert_eq!(cursors(&raw), Err(Failure("setup_wake_unavailable")));
        }
        Ok(())
    }
}
