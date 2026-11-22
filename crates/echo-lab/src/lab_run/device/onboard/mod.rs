//! A bridge that runs on the Echo: configured, launched and read back over adb.
//! Device paths and commands come from the nominated launcher file, never from here.
mod budget;
mod launch;
mod link;
pub(super) mod operator;
#[path = "../runtime/ownership.rs"]
mod ownership;
mod query;
pub(super) mod replay;
#[cfg(test)]
mod tests;

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) use budget::admit;

const INVALID: &str = "device: invalid on-device launcher";
// Group input (AID_INPUT 1004) lets the bridge read the camera shutter evdev node
// (docs/adr/0033-native-camera-shutter.md). The older `3990,inet` spelling is still
// accepted from a lab config and normalizes to this identity.
const IDENTITY: &str = "/data/local/hassmic/runas nobody 3990,inet,input";
const ROOT_REFUSAL: &str = "device: on-device launcher must run as nobody, not root";
const POLL: Duration = Duration::from_millis(200);
const SETTLE: Duration = Duration::from_secs(10);
// Launch contract (docs/architecture.md): own session, stdin held open by a FIFO
// writer, owner-only files. Every name is relative to the nominated private directory.
const START: &str = "umask 077; cd {private_dir} || exit 1; \
setsid sh -c \"sleep 2147483647 >lab-stdin & echo \\$! >lab-holder.pid; \
exec {binary} --config {config} --trace {trace} <lab-stdin >lab-stdout.log 2>lab-stderr.log\" \
</dev/null >/dev/null 2>&1 &";
// Closing the FIFO's write end is the bridge's clean-stop request.
// An empty PID file cannot prove that the detached launch has finished acquiring
// its holder. Refuse cleanup rather than race a late launch after a lost reply.
const STOP: &str = "cd {private_dir} && if test -s lab-holder.pid; then kill $(cat lab-holder.pid); else exit 1; fi";

/// Every device effect of on-device mode crosses this boundary; tests supply a fake.
pub trait Shell {
    /// Runs adb with these arguments and returns its standard output.
    /// # Errors
    /// The command could not run, failed, or exceeded its output or time limit.
    fn adb(&self, args: &[String]) -> Result<Vec<u8>, String>;
}

pub struct Adb(PathBuf);

impl Adb {
    pub fn new(path: &Path) -> Self {
        Self(path.to_owned())
    }
}

impl Shell for Adb {
    fn adb(&self, args: &[String]) -> Result<Vec<u8>, String> {
        // Polling loops own cancellation; cleanup must still reach the device.
        super::super::run_command::controlled(&self.0, args, std::time::Instant::now() + Duration::from_secs(20), None)
    }
}

fn process() -> String {
    "echo-bridge".into()
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launcher {
    private_dir: String,
    config: String,
    trace: String,
    hassmic_log: String,
    #[serde(default = "process")]
    process: String,
    #[serde(default)]
    run_as: String,
    binary: Option<String>,
    start: Option<Vec<String>>,
    stop: Option<Vec<String>>,
}

fn word(text: &str, extra: &[u8]) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte) || extra.contains(&byte))
}

// Paths are substituted into device shell commands, so only inert characters pass.
fn device_path(path: &str) -> bool {
    path.len() <= 200
        && path.starts_with('/')
        && !path.ends_with('/')
        && !path.contains("//")
        && path[1..].split('/').all(|part| part != ".." && word(part, b""))
}

impl Launcher {
    /// # Errors
    /// The flags do not select on-device mode coherently, or the launcher file is invalid.
    pub fn select(path: Option<&String>, bridge: bool, real: bool) -> Result<Option<Self>, String> {
        let Some(path) = path else {
            return Ok(None);
        };
        if bridge || !real {
            return Err(
                "device: --device-bridge replaces --bridge and requires --bridge-config with --live real".into(),
            );
        }
        let bytes = std::fs::read(path).map_err(|_| INVALID)?;
        if bytes.len() > 65_536 {
            return Err(INVALID.into());
        }
        let launcher: Self = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
        launcher.validate()?;
        Ok(Some(launcher))
    }

    fn validate(&self) -> Result<(), String> {
        let identity: Vec<_> = self.run_as.split_whitespace().collect();
        if matches!(
            identity.as_slice(),
            ["runas" | "/data/local/hassmic/runas", "root" | "0", ..]
        ) {
            return Err(ROOT_REFUSAL.into());
        }
        let inside = |path: &str| {
            device_path(path)
                && path
                    .strip_prefix(&self.private_dir)
                    .is_some_and(|rest| rest.starts_with('/'))
        };
        let commands = match (&self.binary, &self.start, &self.stop) {
            (Some(binary), None, None) => device_path(binary),
            (None, Some(start), Some(stop)) => [start, stop].iter().all(|args| {
                matches!(args.as_slice(), [kind, payload] if kind == "shell" && !payload.is_empty() && !payload.contains('\''))
            }),
            _ => false,
        };
        let identity = identity.is_empty()
            || matches!(
                identity.as_slice(),
                [
                    "runas" | "/data/local/hassmic/runas",
                    "nobody",
                    "3990,inet,input" | "3990,inet"
                ]
            );
        if device_path(&self.private_dir)
            && inside(&self.config)
            && inside(&self.trace)
            && device_path(&self.hassmic_log)
            && word(&self.process, b"")
            && self.process.len() <= 15
            && identity
            && commands
        {
            Ok(())
        } else {
            Err(INVALID.into())
        }
    }

    fn prefix() -> String {
        format!("{IDENTITY} ")
    }

    fn fill(&self, text: &str) -> String {
        text.replace("{private_dir}", &self.private_dir)
            .replace("{config}", &self.config)
            .replace("{trace}", &self.trace)
            .replace("{binary}", self.binary.as_deref().unwrap_or_default())
    }

    fn command(&self, nominated: Option<&Vec<String>>, default: &str) -> Vec<String> {
        let payload = nominated.map_or(default, |args| args[1].as_str());
        vec![
            "shell".into(),
            format!("{}sh -c '{}'", Self::prefix(), self.fill(payload)),
        ]
    }
}

pub struct Remote {
    launcher: Launcher,
    shell: Box<dyn Shell>,
    cursor: usize,
    forwards: Vec<(u16, u16)>,
    reverses: Vec<u16>,
    link_mark: u64,
    wake_mark: u64,
    owned: ownership::Owned,
    attempted: bool,
}

impl Remote {
    pub fn new(launcher: Launcher, shell: Box<dyn Shell>) -> Self {
        Self {
            launcher,
            shell,
            cursor: 0,
            forwards: Vec::new(),
            reverses: Vec::new(),
            link_mark: 0,
            wake_mark: 0,
            owned: ownership::Owned::default(),
            attempted: false,
        }
    }

    fn shell(&self, command: &str) -> Result<String, String> {
        let output = self
            .shell
            .adb(&["shell".into(), format!("{}{command}", Launcher::prefix())])?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    // A missing file reads as empty: adb's raw channel carries no exit status.
    fn read(&self, command: &str) -> Result<Vec<u8>, String> {
        self.shell.adb(&[
            "exec-out".into(),
            format!("{}{command} 2>/dev/null", Launcher::prefix()),
        ])
    }

    pub fn trace(&self) -> Result<Vec<u8>, String> {
        self.read(&format!("cat {}", self.launcher.trace))
    }

    /// Liveness by process name: a command-line match would also find the launching shell.
    pub fn pid(&self) -> Result<Option<u32>, String> {
        let output = self
            .shell
            .adb(&["shell", "ps", "-A", "-o", "PID,NAME"].map(String::from))?;
        let text = std::str::from_utf8(&output).map_err(|_| "device: malformed process list")?;
        let mut lines = text.lines();
        if lines.next().map(|line| line.split_whitespace().collect::<Vec<_>>()) != Some(vec!["PID", "NAME"]) {
            return Err("device: malformed process header".into());
        }
        let mut found = Vec::new();
        for line in lines {
            if let [pid, name] = line.split_whitespace().collect::<Vec<_>>()[..]
                && name == self.launcher.process
            {
                found.push(pid.parse::<u32>().map_err(|_| "device: invalid process identity")?);
            }
        }
        match found[..] {
            [] => Ok(None),
            [pid] => Ok(Some(pid)),
            _ => Err("device: unmet prerequisite: exactly one bridge process on the Echo".into()),
        }
    }

    async fn settle(&self, alive: bool, error: &str) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + SETTLE;
        loop {
            if self.pid()?.is_some() == alive {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(error.into());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    pub async fn start(&mut self) -> Result<(), String> {
        if self.pid()?.is_some() {
            return Err("device: refusing to replace an existing bridge process".into());
        }
        self.check_owned()?;
        self.shell(&format!("sh -c ': > {}'", self.launcher.trace))?;
        self.shell(&format!("sh -c ': > {}/lab-holder.pid'", self.launcher.private_dir))?;
        self.cursor = 0;
        self.link_mark = self.log_size()?;
        self.attempted = true;
        self.shell
            .adb(&self.launcher.command(self.launcher.start.as_ref(), START))?;
        self.settle(
            true,
            "device: unmet prerequisite: bridge process on the Echo after launch",
        )
        .await
    }

    pub async fn stop(&mut self) -> Result<(), String> {
        if !self.attempted {
            return Ok(());
        }
        self.check_owned()?;
        self.shell
            .adb(&self.launcher.command(self.launcher.stop.as_ref(), STOP))?;
        self.settle(
            false,
            "device: unmet prerequisite: bridge on the Echo stopped by the launcher; stop any other bridge there first",
        )
        .await?;
        self.attempted = false;
        Ok(())
    }

    /// The screen listens on the Echo's loopback; an owned forward reaches it.
    pub fn reach(&mut self, address: SocketAddr) -> Result<SocketAddr, String> {
        let known = self.forwards.iter().find(|(device, _)| *device == address.port());
        let local = if let Some((_, local)) = known {
            *local
        } else {
            let output = self
                .shell
                .adb(&["forward".into(), "tcp:0".into(), format!("tcp:{}", address.port())])?;
            let local = std::str::from_utf8(&output)
                .ok()
                .map(str::trim)
                .filter(|text| text.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|text| text.parse::<u16>().ok())
                .filter(|port| *port != 0)
                .ok_or("device: malformed owned forward port; coordinator recovery required")?;
            self.forwards.push((address.port(), local));
            local
        };
        Ok(SocketAddr::from((Ipv4Addr::LOCALHOST, local)))
    }

    /// The spoof MCP listens on the bench host; the bridge reaches it on the Echo's loopback.
    pub fn reverse(&mut self, port: u16) -> Result<(), String> {
        let endpoint = format!("tcp:{port}");
        self.shell.adb(&["reverse".into(), endpoint.clone(), endpoint])?;
        self.reverses.push(port);
        Ok(())
    }

    pub fn release(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        for (_, local) in std::mem::take(&mut self.forwards) {
            let args = ["forward".into(), "--remove".into(), format!("tcp:{local}")];
            failures.extend(self.shell.adb(&args).err());
        }
        for port in std::mem::take(&mut self.reverses) {
            let args = ["reverse".into(), "--remove".into(), format!("tcp:{port}")];
            failures.extend(self.shell.adb(&args).err());
        }
        failures.extend(self.release_owned().err());
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}
