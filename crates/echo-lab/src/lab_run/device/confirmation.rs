//! One fresh, consumed file per physical instruction; time is injected into the gate.
use std::{
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

const FAILURE: &str = "operator step not confirmed";
const INSTRUCTION: &str = "Press the mute button, then create the operator confirmation file.";

#[derive(Default)]
struct Gate {
    consumed: bool,
}

impl Gate {
    fn poll(&mut self, elapsed: u64, cancelled: bool, present: bool) -> Result<bool, String> {
        if cancelled {
            return Err("run cancelled".into());
        }
        if elapsed >= 120_000 {
            return Err(FAILURE.into());
        }
        if self.consumed {
            return Ok(false);
        }
        self.consumed = present;
        Ok(present)
    }
}

fn present(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Ok(_) | Err(_) => Err(FAILURE.into()),
    }
}

fn consume(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|_| FAILURE.into())
}

pub fn wait(path: Option<&Path>, cancel: Option<&Path>, errors: &mut dyn Write) -> Result<u64, String> {
    if super::super::run_command::cancelled(cancel) {
        return Err("run cancelled".into());
    }
    // Discard the previous instruction's file before exposing this instruction.
    if let Some(path) = path
        && present(path)?
    {
        consume(path)?;
    }
    let began = Instant::now();
    writeln!(errors, "{INSTRUCTION}")
        .and_then(|()| errors.flush())
        .map_err(|_| "device: operator instruction failed")?;
    let path = path.ok_or(FAILURE)?;
    let mut gate = Gate::default();
    loop {
        let elapsed = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
        // Cancellation and deadline win over even an invalid or fresh file.
        gate.poll(elapsed, super::super::run_command::cancelled(cancel), false)?;
        if gate.poll(elapsed, false, present(path)?)? {
            consume(path)?;
            gate.poll(
                u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX),
                super::super::run_command::cancelled(cancel),
                false,
            )?;
            return Ok(elapsed);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_confirmation_deadline_and_cancellation_have_priority() {
        let mut gate = Gate::default();
        assert_eq!(gate.poll(119_999, false, false), Ok(false));
        assert_eq!(gate.poll(120_000, false, true), Err(FAILURE.into()));
        assert_eq!(gate.poll(120_000, true, true), Err("run cancelled".into()));
        assert_eq!(gate.poll(10, true, true), Err("run cancelled".into()));
    }

    #[test]
    fn device_confirmation_is_single_use() {
        let mut gate = Gate::default();
        assert_eq!(gate.poll(119_999, false, true), Ok(true));
        assert_eq!(gate.poll(119_999, false, true), Ok(false));
    }

    #[test]
    fn device_confirmation_missing_option_prompts_once_and_fails() {
        let mut output = Vec::new();
        assert_eq!(wait(None, None, &mut output), Err(FAILURE.into()));
        assert_eq!(output, format!("{INSTRUCTION}\n").into_bytes());
    }

    #[test]
    fn device_confirmation_regular_file_is_consumed_and_invalid_input_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("confirm");
        std::fs::write(&file, "").unwrap();
        assert_eq!(present(&file), Ok(true));
        consume(&file).unwrap();
        assert_eq!(present(&file), Ok(false));
        assert_eq!(present(root.path()), Err(FAILURE.into()));
    }
    #[test]
    fn device_confirmation_discards_stale_file_before_prompt_and_requires_fresh_file() {
        struct Operator<'a> {
            file: &'a Path,
            cancel: &'a Path,
            fresh: bool,
        }
        impl Write for Operator<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                assert!(!self.file.exists(), "stale file survived arming");
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                std::fs::write(if self.fresh { self.file } else { self.cancel }, "")
            }
        }
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("confirm");
        let cancel = root.path().join("cancel");
        std::fs::write(&file, "stale").unwrap();
        let mut operator = Operator {
            file: &file,
            cancel: &cancel,
            fresh: true,
        };
        assert!(wait(Some(&file), Some(&cancel), &mut operator).unwrap() < 120_000);
        assert!(!file.exists());
        std::fs::write(&file, "previous instruction").unwrap();
        operator.fresh = false;
        assert_eq!(
            wait(Some(&file), Some(&cancel), &mut operator),
            Err("run cancelled".into())
        );
        assert!(!file.exists());
    }
}
