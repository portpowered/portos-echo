use std::{
    io::Read,
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus, Stdio},
};

use echo_boot::{ClockStatus, Result};

use super::{launcher, processes};

#[derive(Default)]
pub(super) struct Jobs {
    device: Option<Child>,
    clock: Option<Child>,
}

impl Jobs {
    pub(super) fn prepare_device(&mut self) -> Result<()> {
        self.device = Some(spawn(
            launcher::command("prepare-device"),
            false,
            "start Android startup services",
        )?);
        Ok(())
    }

    pub(super) fn poll_device(&mut self) -> Result<()> {
        if let Some((_, status)) = completed(&mut self.device, "observe Android startup services")?
            && !status.success()
        {
            return Err("Android startup services failed");
        }
        Ok(())
    }

    pub(super) fn stop_device(&mut self) -> Result<()> {
        stop(&mut self.device)
    }

    pub(super) fn start_clock(&mut self, command: Command) -> Result<()> {
        self.clock = Some(spawn(command, true, "start clock sync")?);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn await_clock_fixture(&mut self) {
        self.clock.as_mut().unwrap().wait().unwrap();
    }

    pub(super) fn poll_clock(&mut self) -> Result<ClockStatus> {
        const STAGE: &str = "clock sync failed";
        let Some((mut child, status)) = completed(&mut self.clock, "observe clock sync")? else {
            return Ok(ClockStatus::Running);
        };
        if !status.success() {
            return Err(STAGE);
        }
        let mut output = String::new();
        child
            .stdout
            .take()
            .ok_or(STAGE)?
            .take(257)
            .read_to_string(&mut output)
            .map_err(|_| STAGE)?;
        let mut fields = output.strip_suffix('\n').ok_or(STAGE)?.split(' ');
        match (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) {
            (Some("clock:"), Some(offset), Some(hosts), Some(action), None)
                if offset
                    .strip_prefix("offset_ms=")
                    .is_some_and(|text| text.parse::<i64>().is_ok())
                    && hosts
                        .strip_prefix("hosts=")
                        .is_some_and(|text| text.parse::<u32>().is_ok()) =>
            {
                match action {
                    "action=set" => Ok(ClockStatus::Adjusted),
                    "action=none" => Ok(ClockStatus::Unchanged),
                    _ => Err(STAGE),
                }
            }
            _ => Err(STAGE),
        }
    }

    pub(super) fn stop_clock(&mut self) -> Result<()> {
        stop(&mut self.clock)
    }
}

fn spawn(mut command: Command, capture: bool, stage: &'static str) -> Result<Child> {
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(if capture { Stdio::piped() } else { Stdio::null() })
        .spawn()
        .map_err(|_| stage)
}

fn completed(job: &mut Option<Child>, stage: &'static str) -> Result<Option<(Child, ExitStatus)>> {
    let Some(child) = job.as_mut() else {
        return Ok(None);
    };
    let status = child.try_wait().map_err(|_| stage)?;
    Ok(status.and_then(|status| job.take().map(|child| (child, status))))
}

fn stop(job: &mut Option<Child>) -> Result<()> {
    if let Some(child) = job.as_mut() {
        processes::kill_job(child)?;
        *job = None;
    }
    Ok(())
}
