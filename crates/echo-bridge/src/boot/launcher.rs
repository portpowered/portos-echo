use std::{
    ffi::OsString,
    fs::OpenOptions,
    io::Write,
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, ExitCode},
};

use echo_boot::{Identity, Result, Role, Slot, layout};
use echo_bridge::bridge_time::clock_command;
use rustix::{
    process::{self, Gid, Signal, Uid},
    thread::{set_thread_groups, set_thread_res_gid, set_thread_res_uid},
};

const SHELL: &str = "/system/bin/sh";
// The fixed /system entry is the only exec applying attr/exec. Pause there
// before the candidate can run so the parent can attest the actual domain.
const DEVICE_TRAMPOLINE: &str = r#"
context=
IFS= read -r context </proc/self/attr/current
[ "$context" = u:r:hassmic:s0 ] || exit 1
kill -STOP "$$" || exit 1
exec "$@"
"#;

pub(super) fn require_root() -> Result<()> {
    if cfg!(target_os = "android") && process::getuid().is_root() && process::geteuid().is_root() {
        Ok(())
    } else {
        Err("requires Android root")
    }
}

pub(super) fn command(action: &str) -> Command {
    let mut command = Command::new("/proc/self/exe");
    command
        .env_clear()
        .env("PATH", "/system/bin:/system/xbin:/vendor/bin")
        .env("ANDROID_ROOT", "/system")
        .env("ANDROID_DATA", "/data")
        .args(["__boot-launch", action, &std::process::id().to_string()]);
    command
}

pub(super) fn child(role: Role, slot: Slot) -> Command {
    let mut command = command(role.name());
    command.arg(slot.name());
    command
}

pub(crate) fn launch(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    super::result(run(args))
}

fn run(args: impl IntoIterator<Item = OsString>) -> Result<()> {
    require_root()?;
    let args = args
        .into_iter()
        .map(OsString::into_string)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| "invalid private launch")?;
    match args.as_slice() {
        [action] if action == "restore-alexa" => {
            let _lock = super::layout_io::Layout::open()?;
            super::device_io::DeviceIo::open()?.restore(super::device_io::log)
        }
        [action, parent] if action == "prepare-device" => {
            guard_parent(parent)?;
            super::device_io::DeviceIo::open()?.prepare(super::device_io::log);
            Ok(())
        }
        [action, parent] if action == "clock" => clock(parent),
        [action, parent] if ["finish", "resume"].contains(&action.as_str()) => {
            drop_identity(Role::Core.identity(), parent)?;
            super::layout_io::acknowledge(action == "finish")
        }
        [action, parent, candidate] => {
            let role = match action.as_str() {
                "device" => Role::Device,
                "core" => Role::Core,
                _ => return Err("invalid private role"),
            };
            let slot = match candidate.as_str() {
                layout::CURRENT => Slot::Current,
                layout::PREVIOUS => Slot::Previous,
                _ => return Err("invalid private slot"),
            };
            enter(role, slot, parent)
        }
        _ => Err("invalid private launch"),
    }
}

fn clock(parent: &str) -> Result<()> {
    drop_identity(
        Identity {
            uid: 0,
            real_gid: 3990,
            effective_gid: 3990,
            groups: &[3990, 3003],
            domain: None,
        },
        parent,
    )?;
    let args = ["clock".to_owned(), "sync".to_owned()];
    let command = clock_command::parse(&args, false)
        .map_err(|_| "prepare clock sync")?
        .ok_or("prepare clock sync")?;
    command.execute(&mut std::io::stdout()).map_err(|_| "clock sync failed")
}

fn enter(role: Role, slot: Slot, parent: &str) -> Result<()> {
    let (path, _) = super::layout_io::candidate(slot)?;
    process::setsid().map_err(|_| "create role session")?;
    let identity = role.identity();
    drop_identity(identity, parent)?;
    match identity.domain {
        Some(domain) => {
            OpenOptions::new()
                .write(true)
                .open("/proc/self/attr/exec")
                .and_then(|mut attr| attr.write_all(domain.as_bytes()))
                .map_err(|_| "set device exec context")?;
            let _error = Command::new(SHELL)
                .args(["-c", DEVICE_TRAMPOLINE, "echo-bridge-boot-device"])
                .arg(path)
                .args(["device", "-P", "wyoming", "-D", "-E"])
                .exec();
        }
        None => {
            process::kill_process(process::getpid(), Signal::STOP).map_err(|_| "pause core launch")?;
            let root = Path::new(layout::ROOT);
            let private = root.join(layout::PRIVATE);
            let _error = Command::new(path)
                .env("HOME", root.join(layout::HOME))
                .arg("--config")
                .arg(private.join(layout::CONFIG))
                .arg("--trace")
                .arg(private.join(layout::TRACE))
                .exec();
        }
    }
    Err("execute selected binary")
}

fn drop_identity(identity: Identity, parent: &str) -> Result<()> {
    // This entry runs immediately after exec of the private launcher, with one thread.
    let groups: Vec<_> = identity.groups.iter().copied().map(Gid::from_raw).collect();
    set_thread_groups(&groups).map_err(|_| "set role supplementary groups")?;
    let effective = Gid::from_raw(identity.effective_gid);
    set_thread_res_gid(Gid::from_raw(identity.real_gid), effective, effective).map_err(|_| "set role gids")?;
    let user = Uid::from_raw(identity.uid);
    set_thread_res_uid(user, user, user).map_err(|_| "set role uid")?;
    guard_parent(parent)
}

fn guard_parent(parent: &str) -> Result<()> {
    let expected_parent = parent
        .parse::<i32>()
        .ok()
        .and_then(process::Pid::from_raw)
        .ok_or("invalid supervisor process")?;
    process::set_parent_process_death_signal(Some(Signal::KILL)).map_err(|_| "guard role parent")?;
    if process::getppid() == Some(expected_parent) {
        Ok(())
    } else {
        Err("role supervisor exited")
    }
}
