use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(crate) struct Options {
    pub adb: PathBuf,
    pub serial: Option<String>,
    pub out: PathBuf,
    pub timeout: Duration,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut flags = std::collections::BTreeMap::new();
        for pair in args.chunks(2) {
            let [flag, value] = pair else {
                return Err("calibrate: flags require values".into());
            };
            if !["--adb", "--serial", "--out", "--timeout-secs"].contains(&flag.as_str())
                || value.is_empty()
                || flags.insert(flag.as_str(), value).is_some()
            {
                return Err("calibrate: invalid or duplicate argument".into());
            }
        }
        let adb = flags.get("--adb").ok_or("calibrate: --adb is required")?;
        let out = flags.get("--out").ok_or("calibrate: --out is required")?;
        let seconds = flags
            .get("--timeout-secs")
            .map_or(Ok(120), |s| s.parse::<u64>())
            .ok()
            .filter(|s| *s > 0)
            .ok_or("calibrate: timeout must be a positive integer")?;
        Ok(Self {
            adb: PathBuf::from(adb),
            serial: flags.get("--serial").map(|s| (*s).clone()),
            out: PathBuf::from(out),
            timeout: Duration::from_secs(seconds),
        })
    }
}

pub(crate) trait Deadline {
    fn remaining(&mut self, complete_taps: usize) -> Duration;
}

pub(super) struct Clock {
    start: Instant,
    budget: Duration,
}

impl Clock {
    #[allow(clippy::disallowed_methods, reason = "Existing host time")]
    pub fn new(budget: Duration) -> Self {
        Self {
            start: Instant::now(),
            budget,
        }
    }
}

impl Deadline for Clock {
    fn remaining(&mut self, _: usize) -> Duration {
        self.budget.saturating_sub(self.start.elapsed())
    }
}
