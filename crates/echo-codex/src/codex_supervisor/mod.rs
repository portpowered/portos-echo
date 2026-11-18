//! Private child ownership. Restarts are explicit and never replay caller work.
mod codex_config;
use crate::{
    codex_io::RpcIo,
    codex_protocol::initialize_params,
    codex_rpc::{RpcError, RpcState},
};
pub use codex_config::SupervisorConfig;
use std::{collections::VecDeque, fmt, fs::File, process::Stdio};
use tokio::{
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{Instant, timeout},
};

/// Payload-free lifecycle diagnostics. Paths, overrides and stderr stay private.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessError {
    InvalidConfig,
    PrivateHome,
    Spawn,
    Unavailable,
    Reap,
    Rpc(RpcError),
}
impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Codex process: {self:?}")
    }
}
impl std::error::Error for ProcessError {}
impl From<RpcError> for ProcessError {
    fn from(value: RpcError) -> Self {
        Self::Rpc(value)
    }
}

/// Owns one child and its IO. `start`/`restart` initialize before exposing IO.
/// The home remains on disk for caller-controlled credential retention/cleanup.
/// Explicit `shutdown` awaits reaping; Drop kills, with Tokio's best-effort reap.
pub struct Supervisor {
    config: SupervisorConfig,
    child: Option<Child>,
    io: Option<RpcIo<ChildStdout, ChildStdin>>,
    restarts: RestartBudget,
    launched: bool,
    ready: bool,
    environment: Vec<(String, String)>,
    lease: File,
}

impl Supervisor {
    /// Reserve a new, private external home and a separate empty working directory.
    /// # Errors
    /// Reject invalid bounds, relative paths, existing homes and repository paths.
    pub fn new(config: SupervisorConfig) -> Result<Self, ProcessError> {
        RpcState::new(config.limits())?;
        if config.restart_window.is_zero() || !config.binary.is_absolute() {
            return Err(ProcessError::InvalidConfig);
        }
        let lease = config.prepare()?;
        Ok(Self {
            config,
            child: None,
            io: None,
            restarts: RestartBudget::default(),
            launched: false,
            ready: false,
            environment: Vec::new(),
            lease,
        })
    }

    /// Supply process-local proxy or certificate settings without changing the
    /// host environment. Provider credentials must use the account RPC surface.
    /// # Errors
    /// Only routing/certificate keys are accepted, and only before first launch.
    pub fn set_environment(&mut self, key: &str, value: String) -> Result<(), ProcessError> {
        let refresh = key == "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
        let refresh_target = value
            .strip_prefix("http://")
            .and_then(|url| url.strip_suffix("/token"))
            .and_then(|authority| {
                authority
                    .strip_prefix("127.0.0.1:")
                    .or_else(|| authority.strip_prefix("[::1]:"))
            })
            .filter(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|port| port.parse::<u16>().ok())
            .is_some_and(|port| port != 0);
        let allowed = matches!(
            key.to_ascii_uppercase().as_str(),
            "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "NO_PROXY" | "SSL_CERT_FILE"
        );
        let duplicate = self.environment.iter().any(|(name, _)| name == key);
        if self.launched || !(allowed || refresh && refresh_target && !duplicate) || value.contains('\0') {
            return Err(ProcessError::InvalidConfig);
        }
        self.environment.push((key.into(), value));
        Ok(())
    }

    /// Launch once. Cancellation leaves a kill-on-drop child owned by this object;
    /// it cannot expose uninitialized IO and must be restarted or shut down.
    /// # Errors
    /// Spawn, initialization and flushed initialized-notification failures.
    pub async fn start(&mut self) -> Result<(), ProcessError> {
        if self.launched {
            return Err(ProcessError::Unavailable);
        }
        self.launched = true;
        self.launch().await
    }

    /// Terminate/reap the previous generation, then initialize a fresh one.
    /// No IDs, pending requests, notifications or tool results cross generations.
    /// # Errors
    /// Exhausted restart budget, cleanup, spawn or handshake failure.
    pub async fn restart(&mut self) -> Result<(), ProcessError> {
        if !self.launched {
            return self.start().await;
        }
        self.shutdown().await?;
        self.restarts
            .admit(Instant::now(), self.config.restart_limit, self.config.restart_window)?;
        self.launch().await
    }

    async fn launch(&mut self) -> Result<(), ProcessError> {
        let mut command = self.command();
        let mut child = command.spawn().map_err(|_| ProcessError::Spawn)?;
        let pipes = child.stdout.take().zip(child.stdin.take());
        self.child = Some(child);
        let result = self.handshake(pipes).await;
        if result.is_err() {
            self.shutdown().await?;
        }
        result
    }

    async fn handshake(&mut self, pipes: Option<(ChildStdout, ChildStdin)>) -> Result<(), ProcessError> {
        let (reader, writer) = pipes.ok_or(ProcessError::Spawn)?;
        self.io = Some(RpcIo::new(reader, writer, self.config.limits())?);
        let io = self.io.as_mut().ok_or(ProcessError::Unavailable)?;
        let id = io.begin_request("initialize", &initialize_params())?;
        io.wait_reply(&id).await?;
        io.notify("initialized")?;
        io.flush().await?;
        self.ready = true;
        Ok(())
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.config.binary);
        command.args(["app-server", "--listen", "stdio://"]);
        for (key, value) in &self.config.config_overrides {
            command.arg("-c").arg(format!("{key}={value}"));
        }
        command.env_clear();
        for key in ["PATH", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.envs(self.environment.iter().map(|(k, v)| (k, v)));
        command.env("CODEX_HOME", &self.config.codex_home);
        command.env("HOME", &self.config.codex_home);
        command.current_dir(self.config.cwd());
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command.kill_on_drop(true);
        command
    }

    /// Access initialized, bounded IO. Poll it to drive requests and deadlines.
    /// A terminal IO failure requires `shutdown` or explicit `restart` before reuse.
    /// # Errors
    /// No successful handshake in the current generation.
    pub fn io(&mut self) -> Result<&mut RpcIo<ChildStdout, ChildStdin>, ProcessError> {
        if !self.ready {
            return Err(ProcessError::Unavailable);
        }
        self.io.as_mut().ok_or(ProcessError::Unavailable)
    }

    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    /// Close pipes, signal termination and await child reaping within the timeout.
    /// # Errors
    /// Failure to terminate/reap; the child stays owned for another cleanup attempt.
    pub async fn shutdown(&mut self) -> Result<(), ProcessError> {
        self.ready = false;
        self.io.take();
        if let Some(child) = &mut self.child {
            child.start_kill().map_err(|_| ProcessError::Reap)?;
            timeout(self.config.request_timeout, child.wait())
                .await
                .map_err(|_| ProcessError::Reap)?
                .map_err(|_| ProcessError::Reap)?;
        }
        self.child.take();
        Ok(())
    }
}

#[derive(Default)]
struct RestartBudget(VecDeque<Instant>);

impl RestartBudget {
    fn admit(&mut self, now: Instant, limit: u32, window: std::time::Duration) -> Result<(), ProcessError> {
        self.0.retain(|at| now.duration_since(*at) < window);
        if self.0.len() >= limit as usize {
            return Err(ProcessError::Unavailable);
        }
        self.0.push_back(now);
        Ok(())
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.io.take();
        let reaped = self.child.as_mut().is_none_or(|child| {
            let _ = child.start_kill();
            matches!(child.try_wait(), Ok(Some(_)))
        });
        if reaped {
            // Unlock the shared open description before any inherited copy closes.
            let _ = self.lease.unlock();
        }
    }
}

#[cfg(test)]
mod codex_restart_tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn drop_releases_home_with_retained_open_description() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::DirBuilderExt;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let parent = std::env::temp_dir().join(format!("echo-lease-{}-{nonce}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&parent)?;
        let binary = std::env::current_exe()?;
        let config = || SupervisorConfig::new(binary.clone(), parent.join("home"));
        let owner = Supervisor::new(config())?;
        let retained = owner.lease.try_clone()?;
        assert!(matches!(Supervisor::new(config()), Err(ProcessError::PrivateHome)));
        drop(owner);
        let next = Supervisor::new(config());
        assert!(
            next.is_ok(),
            "dropped owner must release home despite retained description"
        );
        let next = next?;
        assert!(matches!(retained.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        drop(next);
        retained.try_lock()?;
        retained.unlock()?;
        drop(retained);
        std::fs::remove_dir_all(parent)?;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn restart_budget_expires_at_window_boundary() -> Result<(), ProcessError> {
        let window = std::time::Duration::from_secs(60);
        let mut budget = RestartBudget::default();
        budget.admit(Instant::now(), 2, window)?;
        budget.admit(Instant::now(), 2, window)?;
        assert_eq!(budget.admit(Instant::now(), 2, window), Err(ProcessError::Unavailable));
        tokio::time::advance(window).await;
        budget.admit(Instant::now(), 2, window)?;
        budget.admit(Instant::now(), 2, window)?;
        assert_eq!(budget.admit(Instant::now(), 2, window), Err(ProcessError::Unavailable));
        assert_eq!(
            RestartBudget::default().admit(Instant::now(), 0, window),
            Err(ProcessError::Unavailable)
        );
        Ok(())
    }
}
