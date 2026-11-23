use super::wire::{Operation, RESULT_LIMIT, Reading};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::{
        fd::OwnedFd,
        unix::{
            fs::{DirBuilderExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub(super) fn observe(socket: &Path, operation: Operation) -> io::Result<Reading> {
    let dex = Helper::new(socket.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
    let (mut local, child) = UnixStream::pair()?;
    local.set_read_timeout(Some(Duration::from_secs(9)))?;
    local.set_write_timeout(Some(Duration::from_secs(1)))?;
    // The native satellite sets SIGCHLD to SIG_IGN and the kernel reaps its
    // children. EOF on the private pipe, including after the helper watchdog,
    // reports failure without inspecting child diagnostics or signalling a PID.
    Command::new("/system/bin/app_process")
        .env("CLASSPATH", dex.path())
        .args(["/", "EchoWifi"])
        .stdin(Stdio::from(OwnedFd::from(child.try_clone()?)))
        .stdout(Stdio::from(OwnedFd::from(child)))
        .stderr(Stdio::null())
        .spawn()?;
    writeln!(local, "{}", operation.name())?;
    local.shutdown(Shutdown::Write)?;
    let mut reply = Vec::new();
    BufReader::new(local.take(RESULT_LIMIT)).read_until(b'\n', &mut reply)?;
    if reply.pop() != Some(b'\n') {
        return Err(io::ErrorKind::InvalidData.into());
    }
    serde_json::from_slice(&reply).map_err(|_| io::ErrorKind::InvalidData.into())
}

struct Helper(PathBuf);

impl Helper {
    fn new(parent: &Path) -> io::Result<Self> {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".wifi-{}-{id}", std::process::id()));
        DirBuilder::new().mode(0o700).create(&path)?;
        let helper = Self(path);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o400)
            .open(helper.path())?
            .write_all(echo_device::WIFI_DEX)?;
        Ok(helper)
    }

    fn path(&self) -> PathBuf {
        self.0.join("helper.dex")
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.path());
        let _ = fs::remove_dir(&self.0);
    }
}
