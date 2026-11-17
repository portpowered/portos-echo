//! Runtime admission is separate from offline configuration parsing.
use crate::Error;
use echo_codex::codex_supervisor::{ProcessError, Supervisor, SupervisorConfig};
use echo_config::CodexConfig;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    process::Command,
    time::{Instant, timeout, timeout_at},
};

/// Own startup, cancellation and awaited reaping around one borrowed generation.
pub(crate) async fn run<T>(
    mut supervisor: Supervisor,
    cancelled: impl std::future::Future<Output = ()>,
    deadline: Instant,
    work: impl AsyncFnOnce(&mut Supervisor) -> Result<T, Error>,
) -> Result<T, Error> {
    let operation = async {
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        timeout_at(deadline, supervisor.start())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(process_error)?;
        work(&mut supervisor).await
    };
    let result = tokio::select! {
        biased;
        () = cancelled => Err(Error::Control),
        result = operation => result,
    };
    // Cancel the borrower before closing pipes and reaping the owned child.
    // The home lease remains held until this owner drops after shutdown.
    supervisor.shutdown().await.map_err(process_error)?;
    result
}

pub(crate) fn process_error(error: ProcessError) -> Error {
    match error {
        ProcessError::PrivateHome => Error::CodexHome,
        ProcessError::InvalidConfig | ProcessError::Spawn => Error::CodexBinary,
        _ => Error::Unavailable,
    }
}

pub(crate) async fn supervisor(config: &CodexConfig) -> Result<Supervisor, Error> {
    let home = config.home.clone().ok_or(Error::CodexHomeRequired)?;
    let binary = match &config.binary {
        Some(binary) => binary.clone(),
        None => resolve_binary().await?,
    };
    executable(&binary)?;
    Supervisor::new(SupervisorConfig::new(binary, home)).map_err(process_error)
}

fn executable(path: &Path) -> Result<(), Error> {
    let metadata = path.metadata().map_err(|_| Error::CodexBinary)?;
    if !path.is_absolute() || !metadata.is_file() {
        return Err(Error::CodexBinary);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(Error::CodexBinary);
        }
    }
    Ok(())
}

async fn resolve_binary() -> Result<PathBuf, Error> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut command = helper(&root);
    command.env("ECHO_CODEX_OFFLINE", "1").env_remove("PSModulePath");
    command.stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    let output = timeout(Duration::from_secs(30), command.output())
        .await
        .map_err(|_| Error::CodexBinary)?
        .map_err(|_| Error::CodexBinary)?;
    if !output.status.success() || output.stdout.len() > 32_768 {
        return Err(Error::CodexBinary);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| Error::CodexBinary)?;
    let path = text.trim();
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(Error::CodexBinary);
    }
    Ok(path.into())
}

#[cfg(windows)]
fn helper(root: &Path) -> Command {
    let mut command = Command::new("powershell.exe");
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"]);
    command.arg(root.join("scripts/codex-app-server.ps1"));
    command
}

#[cfg(not(windows))]
fn helper(root: &Path) -> Command {
    let mut command = Command::new("sh");
    command.arg(root.join("scripts/codex-app-server.sh"));
    command
}
