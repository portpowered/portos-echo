//! Runtime admission is separate from offline configuration parsing.
use crate::Error;
use echo_codex::codex_supervisor::{Launch, ProcessError, Supervisor, SupervisorConfig};
use echo_config::{CodexConfig, CodexLaunch};
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
    routed_supervisor(config, None).await
}

pub(crate) async fn routed_supervisor(
    config: &CodexConfig,
    routes: Option<&super::LocalRoutes>,
) -> Result<Supervisor, Error> {
    let home = config.home.clone().ok_or(Error::CodexHomeRequired)?;
    let binary = match &config.binary {
        Some(binary) => binary.clone(),
        None => resolve_binary().await?,
    };
    executable(&binary)?;
    let certificates = config.ca_file.as_deref().map(certificates).transpose()?;
    let launch = match config.launch {
        CodexLaunch::Subcommand => Launch::Subcommand,
        CodexLaunch::Standalone => Launch::Standalone,
    };
    let mut config = SupervisorConfig::new(binary, home);
    config.launch = launch;
    if let Some(routes) = routes {
        routes.configure(&mut config);
    }
    let mut supervisor = Supervisor::new(config).map_err(process_error)?;
    if let Some(certificates) = certificates {
        // The child's environment is cleared, so it finds no system bundle by itself.
        supervisor
            .set_environment("SSL_CERT_FILE", certificates)
            .map_err(|_| Error::CodexCaFile)?;
    }
    if let Some(routes) = routes {
        routes.environment(&mut supervisor)?;
    }
    Ok(supervisor)
}

/// Runtime admission of `codex.ca_file`: an absolute, existing regular file. Only the child reads it.
fn certificates(path: &Path) -> Result<String, Error> {
    let metadata = path.metadata().map_err(|_| Error::CodexCaFile)?;
    if !path.is_absolute() || !metadata.is_file() {
        return Err(Error::CodexCaFile);
    }
    path.to_str().map(str::to_owned).ok_or(Error::CodexCaFile)
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

fn helper(root: &Path) -> Command {
    let mut command = Command::new("sh");
    command.arg(root.join("scripts/codex-app-server.sh"));
    command
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    /// A standalone app server: no subcommand. It records its launch and answers `initialize`.
    const STANDALONE: &str = r#"#!/bin/sh
printf '%s\n' "$*" >"$CODEX_HOME/argv"
printf '%s\n' "$SSL_CERT_FILE" >"$CODEX_HOME/ca"
read -r request
id=$(printf '%s' "$request" | sed 's/.*"id":\([0-9]*\).*/\1/')
printf '{"id":%s,"result":{}}\n' "$id"
cat >/dev/null
"#;

    fn device(root: &Path) -> CodexConfig {
        let private = root.join("private");
        std::fs::DirBuilder::new().mode(0o700).create(&private).unwrap();
        let binary = root.join("codex-app-server");
        std::fs::write(&binary, STANDALONE).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.join("ca.pem"), "synthetic bundle; never parsed here").unwrap();
        CodexConfig {
            home: Some(private.join("home")),
            binary: Some(binary),
            launch: CodexLaunch::Standalone,
            ca_file: Some(root.join("ca.pem")),
            ..CodexConfig::default()
        }
    }

    /// T3: one local child process, no network.
    #[tokio::test]
    async fn t3_standalone_launch_reaches_the_child_without_a_subcommand_and_with_the_ca_bundle() {
        let root = tempfile::tempdir().unwrap();
        let config = device(root.path());
        let home = config.home.clone().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let started = run(
            supervisor(&config).await.unwrap(),
            std::future::pending(),
            deadline,
            async |supervisor| Ok(supervisor.pid().is_some()),
        )
        .await;
        assert_eq!(started, Ok(true));
        assert_eq!(
            std::fs::read_to_string(home.join("argv")).unwrap(),
            "--listen stdio://\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("ca")).unwrap(),
            format!("{}\n", root.path().join("ca.pem").display())
        );
    }

    #[tokio::test]
    async fn ca_file_must_be_an_existing_regular_file_before_the_home_is_claimed() {
        let root = tempfile::tempdir().unwrap();
        let mut config = device(root.path());
        for refused in [root.path().join("absent.pem"), root.path().join("private")] {
            config.ca_file = Some(refused);
            assert!(matches!(supervisor(&config).await, Err(Error::CodexCaFile)));
        }
        assert!(!config.home.clone().unwrap().exists());
        assert_eq!(Error::CodexCaFile.to_string(), "codex.ca_file: unavailable");
    }
}
