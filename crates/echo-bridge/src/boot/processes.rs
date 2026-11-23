use std::{
    fs,
    process::{Child, Stdio},
    sync::{Arc, OnceLock},
    thread,
    time::{Duration, Instant},
};

use echo_boot::{ChildStatus, Identity, Result, Role, Slot, Stop};
use rustix::{
    io::Errno,
    process::{self, Pid, Signal, WaitId, WaitIdOptions, WaitOptions},
};

const CHECK: Duration = Duration::from_millis(10);
const LAUNCH: Duration = Duration::from_secs(2);
const GRACE: Duration = Duration::from_secs(2);
const REAP: Duration = Duration::from_secs(2);

pub(super) struct RoleChild {
    child: Child,
    ready: Arc<OnceLock<Instant>>,
}

impl RoleChild {
    pub(super) fn spawn(role: Role, slot: Slot, identity: Identity) -> Result<Self> {
        let core = role == Role::Core;
        let child = super::launcher::child(role, slot)
            .stdin(if core { Stdio::piped() } else { Stdio::null() })
            .stdout(if core { Stdio::piped() } else { Stdio::inherit() })
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|_| "spawn role launcher")?;
        let mut role_child = Self {
            child,
            ready: Arc::new(OnceLock::new()),
        };
        if let Err(error) = role_child.admit(identity) {
            role_child.stop(Stop::Shutdown)?;
            return Err(error);
        }
        Ok(role_child)
    }

    fn pid(&self) -> Result<Pid> {
        i32::try_from(self.child.id())
            .ok()
            .and_then(Pid::from_raw)
            .ok_or("invalid role process")
    }

    fn admit(&mut self, identity: Identity) -> Result<()> {
        let pid = self.pid()?;
        let deadline = Instant::now() + LAUNCH;
        loop {
            let state = process::waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::STOPPED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )
            .map_err(|_| "observe role launch")?;
            if let Some(state) = state {
                if !state.stopped() {
                    return Err("role setup failed");
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err("role setup timed out");
            }
            thread::sleep(CHECK);
        }
        attest(pid, identity)?;
        if let Some(stdout) = self.child.stdout.take() {
            let ready = Arc::clone(&self.ready);
            thread::Builder::new()
                .name("boot-ready".into())
                .spawn(move || {
                    let _ = super::receipt::drain(stdout, &ready);
                })
                .map_err(|_| "drain core output")?;
        }
        process::kill_process(pid, Signal::CONT).map_err(|_| "resume role launch")
    }

    pub(super) fn observe(&self, origin: Instant) -> Result<ChildStatus> {
        if exited(self.pid()?)? {
            Ok(ChildStatus::Exited)
        } else {
            Ok(ChildStatus::Running {
                ready_at: self.ready.get().map(|at| at.duration_since(origin)),
            })
        }
    }

    pub(super) fn stop(&mut self, why: Stop) -> Result<String> {
        let pid = self.pid()?;
        let grouped = process::getpgid(Some(pid)) == Ok(pid);
        let core = self.child.stdin.take().is_some();
        if why != Stop::Exited && !exited(pid)? {
            if !core {
                signal(pid, grouped, Signal::TERM)?;
            }
            let _ = until(Instant::now() + GRACE, || exited(pid))?;
        }
        // The unreaped leader reserves this PID across the last group signal.
        signal(pid, grouped, Signal::KILL)?;
        let reap_deadline = Instant::now() + REAP;
        if !until(reap_deadline, || exited(pid))? {
            return Err("role did not exit");
        }
        let status = self.child.wait().map_err(|_| "reap role")?;
        if grouped && !until(reap_deadline, || group_reaped(pid))? {
            return Err("role descendants did not exit");
        }
        Ok(status.to_string())
    }
}

fn attest(pid: Pid, identity: Identity) -> Result<()> {
    let path = format!("/proc/{}", pid.as_raw_nonzero());
    let status = fs::read_to_string(format!("{path}/status")).map_err(|_| "inspect role credentials")?;
    let ids = |name: &str| -> Option<Vec<u32>> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))?
            .split_ascii_whitespace()
            .map(|value| value.parse().ok())
            .collect()
    };
    let gids = [
        identity.real_gid,
        identity.effective_gid,
        identity.effective_gid,
        identity.effective_gid,
    ];
    let mut groups = identity.groups.to_vec();
    groups.sort_unstable();
    if process::getpgid(Some(pid)) != Ok(pid)
        || ids("Uid:").as_deref() != Some(&[identity.uid; 4])
        || ids("Gid:").as_deref() != Some(&gids)
        || ids("Groups:").as_deref() != Some(&groups)
    {
        return Err("role credential attestation failed");
    }
    if let Some(domain) = identity.domain {
        let actual = fs::read_to_string(format!("{path}/attr/current")).map_err(|_| "inspect device context")?;
        if actual.trim_end_matches(['\n', '\0']) != domain {
            return Err("device context attestation failed");
        }
    }
    Ok(())
}

fn exited(pid: Pid) -> Result<bool> {
    process::waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .map(|status| status.is_some())
    .map_err(|_| "observe role process")
}

fn signal(pid: Pid, grouped: bool, signal: Signal) -> Result<()> {
    let outcome = if grouped {
        process::kill_process_group(pid, signal)
    } else {
        process::kill_process(pid, signal)
    };
    match outcome {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(_) => Err("signal role process"),
    }
}

fn group_reaped(pid: Pid) -> Result<bool> {
    loop {
        match process::waitpgid(pid, WaitOptions::NOHANG) {
            Ok(Some(_)) => (),
            Ok(None) => return Ok(false),
            Err(Errno::CHILD) => return Ok(process::test_kill_process_group(pid) == Err(Errno::SRCH)),
            Err(_) => return Err("reap role descendants"),
        }
    }
}

fn until(deadline: Instant, mut ready: impl FnMut() -> Result<bool>) -> Result<bool> {
    loop {
        if ready()? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(CHECK);
    }
}

pub(super) fn kill_job(child: &mut Child) -> Result<()> {
    let pid = i32::try_from(child.id())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or("invalid boot job process")?;
    signal(pid, true, Signal::KILL)?;
    let deadline = Instant::now() + REAP;
    if !until(deadline, || exited(pid))? {
        return Err("boot job did not exit");
    }
    child.wait().map_err(|_| "reap boot job")?;
    if !until(deadline, || group_reaped(pid))? {
        return Err("boot job descendants did not exit");
    }
    Ok(())
}
