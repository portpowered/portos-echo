use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    process::Stdio,
    time::Duration,
};

use echo_boot::Result;
use rustix::{
    fs::{XattrFlags, fchown, fgetxattr, fsetxattr},
    mount::{MountFlags, mount_remount},
    process::{Gid, Uid, geteuid, getuid},
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    runtime::{Builder, Runtime},
};

use super::{
    Install,
    files::{self, Files, Owner},
    policy::{self, Tool},
    transaction::{self, RebootFailure, Service, Step, System, Volume},
};

const DEADLINE: Duration = Duration::from_secs(10);
const POLICY_DEADLINE: Duration = Duration::from_secs(30);

struct Android {
    runtime: Runtime,
}

pub(super) fn run(input: Install) -> Result<&'static str> {
    if !(cfg!(target_os = "android") && getuid().is_root() && geteuid().is_root()) {
        return Err("requires Android root");
    }
    let root = Path::new("/");
    let files = Files {
        root,
        root_uid: 0,
        set_owner,
    };
    let mut android = Android {
        runtime: Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "open installer command loop")?,
    };
    let model = android.property("ro.product.device")?;
    let build = android.property("ro.build.display.id")?;
    if model != "athena" || !matches!(build.trim(), "PS75751.7809N" | "PS75741.7080N") {
        return Err("unsupported Athena firmware");
    }
    let enforcing = fs::read_to_string("/sys/fs/selinux/enforce").map_err(|_| "inspect SELinux enforcement")?;
    if enforcing.trim() != "1" {
        return Err("A2 requires enforcing SELinux");
    }
    transaction::run(&files, &mut android, &input)
}

pub(super) fn set_owner(file: &File, Owner(uid, gid): Owner) -> Result<()> {
    fchown(file, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid))).map_err(|_| "set install ownership")
}

impl Android {
    fn property(&self, key: &str) -> Result<String> {
        let value = self.command(
            Path::new("/system/bin/getprop"),
            &[Path::new(key)],
            1024,
            DEADLINE,
            "read Android property",
        )?;
        Ok(value.trim().to_owned())
    }

    fn command(
        &self,
        tool: &Path,
        args: &[&Path],
        limit: u64,
        deadline: Duration,
        failure: &'static str,
    ) -> Result<String> {
        self.runtime.block_on(async {
            let mut child = Command::new(tool)
                .args(args)
                .env_clear()
                .env("PATH", "/system/bin:/system/xbin:/vendor/bin")
                .env("ANDROID_ROOT", "/system")
                .env("ANDROID_DATA", "/data")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .stdout(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|_| failure)?;
            let output = child.stdout.take().ok_or(failure)?;
            let mut buffer = String::new();
            let mut reader = output.take(limit + 1);
            let exchange = async {
                reader.read_to_string(&mut buffer).await.map_err(|_| failure)?;
                if buffer.len() as u64 > limit || !child.wait().await.map_err(|_| failure)?.success() {
                    return Err(failure);
                }
                Ok(buffer)
            };
            match tokio::time::timeout(deadline, exchange).await {
                Ok(Ok(buffer)) => Ok(buffer),
                _ => {
                    let _ = child.kill().await;
                    Err(failure)
                }
            }
        })
    }
}

impl Tool for Android {
    fn verify_policy_tool(&self, files: &Files<'_>, path: &Path) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let mut file = files.trusted_file(path)?;
        let metadata = file.metadata().map_err(|_| "inspect policy tool")?;
        if metadata.len() > 1024 * 1024 || metadata.mode() & 0o100 == 0 {
            return Err("invalid policy tool");
        }
        let mut elf = [0; 20];
        file.read_exact(&mut elf).map_err(|_| "read policy tool header")?;
        if &elf[..6] != b"\x7fELF\x01\x01" || elf[18..] != [40, 0] {
            return Err("policy tool requires ARM ELF32");
        }
        if files::digest(files.trusted_file(path)?)? != policy::SHA256 {
            return Err("magiskpolicy checksum mismatch");
        }
        Ok(())
    }

    fn policy(&mut self, tool: &Path, args: &[&Path], print: bool) -> Result<String> {
        let cap = if print { 32 * 1024 * 1024 } else { 4096 };
        self.command(tool, args, cap, POLICY_DEADLINE, "apply Athena policy")
    }
}

impl System for Android {
    fn readonly(&mut self, volume: Volume) -> Result<bool> {
        Ok(mount_state(volume)?.1.contains(MountFlags::RDONLY))
    }

    fn remount(&mut self, volume: Volume, readonly: bool) -> Result<()> {
        let (target, mut flags) = mount_state(volume)?;
        flags.set(MountFlags::RDONLY, readonly);
        mount_remount(&target, flags, "").map_err(|_| "remount Athena volume")?;
        if self.readonly(volume)? != readonly {
            return Err("verify Athena remount");
        }
        Ok(())
    }

    fn service(&mut self, name: &'static str) -> Result<Service> {
        match self.property(&format!("init.svc.{name}"))?.as_str() {
            "running" => Ok(Service::Running),
            "stopped" => Ok(Service::Stopped),
            "" => Ok(Service::Absent),
            _ => Err("unknown legacy init service state"),
        }
    }

    fn set_service(&mut self, name: &'static str, state: Service) -> Result<()> {
        let action = match state {
            Service::Running => "ctl.start",
            Service::Stopped => "ctl.stop",
            Service::Absent => return Err("cannot change missing init service"),
        };
        let _output = self.command(
            Path::new("/system/bin/setprop"),
            &[Path::new(action), Path::new(name)],
            1024,
            DEADLINE,
            "change legacy init service",
        )?;
        let deadline = std::time::Instant::now() + DEADLINE;
        while std::time::Instant::now() < deadline {
            if self.service(name) == Ok(state) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err("legacy init service did not settle")
    }

    fn label(&mut self, path: &Path, original: Option<&Path>) -> Result<()> {
        use rustix::fs::CWD;
        let mut buffer = [0; 512];
        let context = if let Some(original) = original {
            let input = files::read_at(CWD, original)?;
            let length = fgetxattr(&input, "security.selinux", &mut buffer[..]).map_err(|_| "read policy context")?;
            &buffer[..length]
        } else {
            b"u:object_r:system_file:s0\0"
        };
        let target = files::read_at(CWD, path)?;
        fsetxattr(&target, "security.selinux", context, XattrFlags::empty()).map_err(|_| "set install file context")?;
        target.sync_all().map_err(|_| "sync install file context")
    }

    fn reboot(&mut self) -> std::result::Result<(), RebootFailure> {
        self.runtime.block_on(async {
            let rejected = RebootFailure::Rejected("submit reboot");
            let mut child = Command::new("/system/bin/reboot")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|_| rejected)?;
            match tokio::time::timeout(DEADLINE, child.wait()).await {
                Ok(Ok(status)) if status.success() => Ok(()),
                Ok(Ok(status)) if status.code().is_some() => Err(RebootFailure::Rejected("submit reboot")),
                _ => Err(RebootFailure::Unknown),
            }
        })
    }

    fn step(&mut self, step: Step, restoring: bool) -> Result<()> {
        let status = if restoring { "restored" } else { "completed" };
        let _ = writeln!(std::io::stderr(), "echo-bridge install: {status} {step:?}");
        Ok(())
    }
}

fn mount_state(volume: Volume) -> Result<(String, MountFlags)> {
    let prefix = match volume {
        Volume::System => "/system",
        Volume::Vendor => "/vendor",
    };
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").map_err(|_| "read Athena mounts")?;
    let (mount, options) = mountinfo
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_ascii_whitespace();
            let mount = fields.nth(4)?;
            let flags = fields.next()?;
            ((mount == prefix) || (mount == "/" && volume == Volume::System)).then_some((mount, flags))
        })
        .max_by_key(|(mount, _)| mount.len())
        .ok_or("unsupported Athena mounts")?;
    let mut flags = MountFlags::empty();
    for option in options.split(',') {
        flags |= match option {
            "ro" => MountFlags::RDONLY,
            "nosuid" => MountFlags::NOSUID,
            "nodev" => MountFlags::NODEV,
            "noexec" => MountFlags::NOEXEC,
            "noatime" => MountFlags::NOATIME,
            "nodiratime" => MountFlags::NODIRATIME,
            "relatime" => MountFlags::RELATIME,
            "strictatime" => MountFlags::STRICTATIME,
            "lazytime" => MountFlags::LAZYTIME,
            "sync" => MountFlags::SYNCHRONOUS,
            "dirsync" => MountFlags::DIRSYNC,
            "rw" | "seclabel" => MountFlags::empty(),
            _ => return Err("unsupported Athena mount option"),
        };
    }
    Ok((mount.to_owned(), flags))
}
