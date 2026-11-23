//! Private Android transport for the native installer. Each fixed operation
//! requires a verified active receipt and takes the installer directory lock.
use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use echo_boot::{Result, Role, layout};
use echo_contracts::secret::Secret;
use echo_core_api::{Clock, SystemClock};
use rustix::{
    fs::{self as unix, Mode, OFlags},
    process::{self, Gid, Uid},
    thread::{set_thread_groups, set_thread_res_gid, set_thread_res_uid},
};

use super::{
    android,
    files::{self, Files},
    payload, transaction,
};

#[path = "host/observations.rs"]
mod observations;
#[path = "host/private.rs"]
mod private;

const ROOT: &str = layout::ROOT;
const BOOT: &str = "/data/local/echo/bin/boot";
const CONFIG: &str = "/data/local/echo/private/bridge.toml";
const ACCOUNT_LIMIT: Duration = Duration::from_secs(300);

pub(super) fn dispatch(child: bool, args: &[OsString]) -> Result<()> {
    if !(cfg!(target_os = "android") && process::getuid().is_root() && process::geteuid().is_root()) {
        return Err("requires Android root");
    }
    let args = args
        .iter()
        .map(|arg| arg.to_str().ok_or("invalid installer operation"))
        .collect::<Result<Vec<_>>>()?;
    if child {
        return run_child(&args);
    }
    let files = Files {
        root: Path::new("/"),
        root_uid: 0,
        set_owner: android::set_owner,
    };
    let lock = files.protected(&files.path("/data/local"))?;
    lock.try_lock_shared().map_err(|_| "installer already running")?;
    let version = transaction::active_release(&files)?;
    run(&files, &version, &args)
}

fn run(files: &Files<'_>, version: &str, args: &[&str]) -> Result<()> {
    match args {
        ["owner"] => output(version),
        ["status"] => output(&observations::status(files, version)?),
        ["doctor"] => output(&observations::doctor(files)?),
        ["setup-config"] => private_read(files, layout::CONFIG, 1024 * 1024),
        ["setup-pairing"] => private_read(files, "screen-pairing.txt", 16_384),
        ["confirm-cursor"] => output(&observations::cursor(files)?),
        ["confirm-check", cursor] => output(observations::confirm(files, cursor)?),
        ["stop"] => {
            let _account = account_lock(files)?;
            private::park(files)?;
            output("echo-install: stopped")
        }
        ["request-park"] => {
            let _account = account_lock(files)?;
            private::request_park(files)
        }
        ["start"] => {
            let _account = account_lock(files)?;
            private::resume(files)
        }
        ["config-write"] => {
            let _account = account_lock(files)?;
            let bytes = read_input(1024 * 1024)?;
            let config = std::str::from_utf8(bytes.expose()).map_err(|_| "invalid setup config")?;
            private::replace(files, layout::CONFIG, payload::migrate_config(config)?.as_bytes())
        }
        ["api-key-write", leaf] => {
            let _account = account_lock(files)?;
            private::replace_key(files, leaf, read_input(16_384)?.expose())
        }
        [operation @ ("signin" | "ensure-signin" | "api-key-status" | "link")] => account(files, operation),
        _ => Err("invalid installer operation"),
    }
}

fn private_read(files: &Files<'_>, leaf: &str, limit: u64) -> Result<()> {
    let value = files::read_text(private::read(files, leaf)?, limit)?;
    write!(std::io::stdout(), "{value}").map_err(|_| "write private projection")
}

fn read_input(limit: u64) -> Result<Secret<Vec<u8>>> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "read private input")?;
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err("invalid private input size");
    }
    Ok(Secret::new(bytes))
}

fn output(line: &str) -> Result<()> {
    writeln!(std::io::stdout(), "{line}").map_err(|_| "write install result")
}

fn account_lock(files: &Files<'_>) -> Result<File> {
    let dir = files.protected(&files.path("/data/local/echo/bin/.install"))?;
    let lock = unix::openat(
        &dir,
        "account.lock",
        OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::RUSR | Mode::WUSR,
    )
    .map(File::from)
    .map_err(|_| "open account lock")?;
    lock.try_lock().map_err(|_| "account operation still active")?;
    // Hold through any child cleanup. An interrupted adb process cannot let
    // another account owner enter while a child still has the lock.
    rustix::io::fcntl_setfd(&lock, rustix::io::FdFlags::empty()).map_err(|_| "inherit account lock")?;
    Ok(lock)
}

fn account(files: &Files<'_>, operation: &str) -> Result<()> {
    let _account = account_lock(files)?;
    let _config = private::read(files, layout::CONFIG)?;
    if operation == "link" {
        if !observations::core_running(files)? {
            return Err("running A2 core required");
        }
    } else {
        private::parked(files)?;
    }
    match operation {
        "ensure-signin" if child_succeeded("status", Duration::from_secs(20))? => (),
        "api-key-status" if child_succeeded("api-key-status", Duration::from_secs(20))? => (),
        "api-key-status" => return Err("bridge did not report API-key mode"),
        "ensure-signin" => {
            require_child("signin")?;
        }
        _ => require_child(operation)?,
    }
    output(&format!("echo-install: {operation} completed"))
}

fn require_child(operation: &str) -> Result<()> {
    if child_succeeded(operation, ACCOUNT_LIMIT)? {
        Ok(())
    } else {
        Err("account operation failed")
    }
}

fn child_succeeded(operation: &str, limit: Duration) -> Result<bool> {
    let mut child = Command::new(BOOT)
        .args(["install", "--host-child", operation])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "start private installer process")?;
    let deadline = SystemClock.now().monotonic + limit;
    loop {
        if let Some(status) = child.try_wait().map_err(|_| "observe private installer process")? {
            return Ok(status.success());
        }
        if SystemClock.now().monotonic >= deadline {
            let leader = i32::try_from(child.id())
                .ok()
                .and_then(process::Pid::from_raw)
                .ok_or("invalid process group")?;
            process::kill_process_group(leader, process::Signal::KILL).map_err(|_| "stop private installer process")?;
            child.wait().map_err(|_| "reap private installer process")?;
            return Err("private installer process timed out");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run_child(args: &[&str]) -> Result<()> {
    let [operation @ ("status" | "signin" | "link" | "api-key-status" | "egress")] = args else {
        return Err("invalid account operation");
    };
    let identity = Role::Core.identity();
    let groups: Vec<_> = identity.groups.iter().copied().map(Gid::from_raw).collect();
    set_thread_groups(&groups).map_err(|_| "set installer groups")?;
    let gid = Gid::from_raw(identity.effective_gid);
    set_thread_res_gid(Gid::from_raw(identity.real_gid), gid, gid).map_err(|_| "set installer gid")?;
    let uid = Uid::from_raw(identity.uid);
    set_thread_res_uid(uid, uid, uid).map_err(|_| "set installer uid")?;
    let command = |program: &str| {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("HOME", Path::new(ROOT).join(layout::HOME))
            .env("PATH", "/system/bin:/system/xbin:/vendor/bin")
            .env("ANDROID_ROOT", "/system")
            .env("ANDROID_DATA", "/data");
        command
    };
    if *operation == "egress" {
        let _error = command("/system/bin/nc")
            .args(["-z", "-w", "5", "chatgpt.com", "443"])
            .exec();
        return Err("execute A2 egress probe");
    }
    let mut bridge = command("/data/local/echo/bin/current");
    let verb = if *operation == "api-key-status" {
        "status"
    } else {
        operation
    };
    bridge.args([verb, "--config", CONFIG]);
    if *operation == "api-key-status" {
        let result = bridge.output().map_err(|_| "inspect API-key mode")?;
        return if result.status.success()
            && result
                .stdout
                .split(|byte| *byte == b'\n')
                .any(|line| line == b"mode: api-key")
        {
            Ok(())
        } else {
            Err("bridge did not report API-key mode")
        };
    }
    let _error = bridge.exec();
    Err("execute A2 account operation")
}
