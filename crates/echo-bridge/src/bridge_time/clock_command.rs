//! `echo-bridge clock check|sync`: measure the system clock, and as root set it.
//!
//! `sync` steps the clock when it is off by at least `--threshold-ms` (default
//! 1000) and is safe to repeat; `check` only measures. Both print one line:
//! `clock: offset_ms=N hosts=N action=set|none|check|denied`, or
//! `clock: unsynced reason=R hosts=N` and a failing exit when the servers did
//! not agree, in which case nothing is set. `offset_ms` is network time minus
//! the system clock. `--config` takes the servers from `[clock]`; without it
//! the built-in defaults are asked, which is what a root caller should use.
//! On the Echo the caller needs group 3990 to get through the device firewall.
use super::sntp::{self, Outcome};
use crate::Error;
use std::{io::Write, path::PathBuf, time::Duration};

const QUERY_TIMEOUT: Duration = Duration::from_millis(1500);
/// The whole measurement, name lookups included.
const BOUND: Duration = Duration::from_secs(5);

pub struct Command {
    sync: bool,
    config: Option<PathBuf>,
    threshold_ms: i64,
}

/// Recognizes the `clock` verb without loading configuration or dialing.
/// # Errors
/// Rejects unknown actions, duplicate or incomplete flags and lab mode.
pub fn parse(args: &[String], lab_present: bool) -> Result<Option<Command>, Error> {
    if args.first().map(String::as_str) != Some("clock") {
        return Ok(None);
    }
    let sync = match args.get(1).map(String::as_str) {
        Some("sync") => true,
        Some("check") => false,
        _ => return Err(Error::Input),
    };
    let (mut config, mut threshold) = (None, None);
    let mut flags = args[2..].iter();
    while let Some(flag) = flags.next() {
        let value = flags.next().filter(|text| !text.is_empty()).ok_or(Error::Input)?;
        let seen = match flag.as_str() {
            "--config" => config.replace(PathBuf::from(value)).is_some(),
            "--threshold-ms" => threshold
                .replace(value.parse::<u32>().map_err(|_| Error::Input)?)
                .is_some(),
            _ => true,
        };
        if seen {
            return Err(Error::Input);
        }
    }
    if lab_present {
        return Err(Error::Input);
    }
    Ok(Some(Command {
        sync,
        config,
        threshold_ms: threshold.map_or(1000, i64::from),
    }))
}

impl Command {
    /// # Errors
    /// `Input` for an unusable configuration; `Unavailable` when the servers did
    /// not agree or the clock could not be set.
    pub fn execute(&self, out: &mut impl Write) -> Result<(), Error> {
        let servers = match &self.config {
            Some(path) => echo_config::BridgeConfig::load(path)
                .map_err(|_| Error::Input)?
                .clock
                .effective_servers(),
            None => echo_config::DEFAULT_TIME_SERVERS.map(String::from).to_vec(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::Unavailable)?;
        let outcome = runtime.block_on(async {
            tokio::time::timeout(BOUND, sntp::measure(&servers, QUERY_TIMEOUT))
                .await
                .unwrap_or(Outcome::Unsynced {
                    reason: "timeout",
                    answered: 0,
                })
        });
        settle(outcome, (self.sync, self.threshold_ms), out, step)
    }
}

/// Move the system clock by `offset_ms`. Needs `CAP_SYS_TIME`.
fn step(offset_ms: i64) -> bool {
    let target = sntp::system_ms().saturating_add(offset_ms);
    let nanoseconds = target.rem_euclid(1000) * 1_000_000;
    rustix::time::clock_settime(
        rustix::time::ClockId::Realtime,
        rustix::time::Timespec {
            tv_sec: target.div_euclid(1000),
            tv_nsec: nanoseconds,
        },
    )
    .is_ok()
}

pub(crate) fn settle(
    outcome: Outcome,
    (sync, threshold_ms): (bool, i64),
    out: &mut impl Write,
    set: impl FnOnce(i64) -> bool,
) -> Result<(), Error> {
    let (offset_ms, hosts) = match outcome {
        Outcome::Synced { offset_ms, hosts } => (offset_ms, hosts),
        Outcome::Unsynced { reason, answered } => {
            writeln!(out, "clock: unsynced reason={reason} hosts={answered}").map_err(|_| Error::Control)?;
            return Err(Error::Unavailable);
        }
    };
    let action = if !sync {
        "check"
    } else if offset_ms.abs() < threshold_ms.max(1) {
        "none"
    } else if set(offset_ms) {
        "set"
    } else {
        "denied"
    };
    writeln!(out, "clock: offset_ms={offset_ms} hosts={hosts} action={action}").map_err(|_| Error::Control)?;
    if action == "denied" {
        return Err(Error::Unavailable);
    }
    Ok(())
}
