use super::wire::{self, Error, RESULT_LIMIT, Reading, Request};
use echo_device::wifi::{self, Join, Status};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
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

pub(super) fn operate(socket: &Path, request: Request) -> Result<Vec<u8>, Error> {
    let op = request.operation();
    let mut radio = Radio::start(socket, matches!(request, Request::Join { .. })).map_err(|_| Error::Unavailable)?;
    if let Request::Join { ssid, password } = request {
        let outcome = wifi::join(&mut radio, &ssid, password.expose());
        return Ok(wire::join_result(outcome));
    }
    let data = request.data();
    if let Request::Forget { .. } = request {
        let reply: Value = radio.call(&data).ok_or(Error::Unavailable)?;
        if reply == json!("not-saved") {
            return Err(Error::NotSaved);
        }
        return serde_json::from_value::<Reading>(reply)
            .map(|status| wire::success(op, status.project()))
            .map_err(|_| Error::Unavailable);
    }
    radio
        .call::<Reading>(&data)
        .map(|reading| wire::success(op, reading.project()))
        .ok_or(Error::Unavailable)
}

struct Radio {
    pipe: BufReader<UnixStream>,
    _dex: Helper,
}

impl Radio {
    fn start(socket: &Path, join: bool) -> io::Result<Self> {
        let dex = Helper::new(socket.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
        let (local, child) = UnixStream::pair()?;
        local.set_read_timeout(Some(Duration::from_secs(if join { 62 } else { 9 })))?;
        local.set_write_timeout(Some(Duration::from_secs(1)))?;
        // The native satellite ignores SIGCHLD and the kernel reaps its children.
        // The helper watchdog closes this private pipe if a framework call hangs.
        Command::new("/system/bin/app_process")
            .env("CLASSPATH", dex.path())
            .args(["/", "EchoWifi"])
            .args(join.then_some(OsStr::new("join")))
            .stdin(Stdio::from(OwnedFd::from(child.try_clone()?)))
            .stdout(Stdio::from(OwnedFd::from(child)))
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self {
            pipe: BufReader::new(local),
            _dex: dex,
        })
    }

    fn call<T: DeserializeOwned>(&mut self, message: &Value) -> Option<T> {
        writeln!(self.pipe.get_mut(), "{message}").ok()?;
        let mut reply = Vec::new();
        (&mut self.pipe).take(RESULT_LIMIT).read_until(b'\n', &mut reply).ok()?;
        if reply.pop() != Some(b'\n') {
            return None;
        }
        serde_json::from_slice(&reply).ok()
    }
}

impl Drop for Radio {
    fn drop(&mut self) {
        let _ = self.pipe.get_ref().shutdown(Shutdown::Both);
    }
}

impl Join for Radio {
    fn prepare(&mut self, ssid: &str, password: &str) -> bool {
        self.call(&json!({"op":"prepare","ssid":ssid,"password":password}))
            .unwrap_or(false)
    }

    fn associate(&mut self) -> Option<Status> {
        self.call::<Reading>(&json!({"op":"associate"})).and_then(as_status)
    }

    fn restore(&mut self) -> Option<Status> {
        self.call::<Reading>(&json!({"op":"restore"})).and_then(as_status)
    }

    fn status(&mut self) -> Option<Status> {
        self.call::<Reading>(&json!({"op":"status"})).and_then(as_status)
    }
}

fn as_status(reading: Reading) -> Option<Status> {
    match reading.project() {
        Reading::Status { ssid, reachable } => Some(Status { ssid, reachable }),
        Reading::Scan { .. } => None,
    }
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
