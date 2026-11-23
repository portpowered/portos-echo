use std::{path::Path, process::Stdio, time::Duration};

use echo_boot::Result;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    runtime::Runtime,
};

const LIMIT: u64 = 128 * 1024;
const DEADLINE: Duration = Duration::from_secs(2);

pub(super) fn run(
    runtime: &Runtime,
    tool: &Path,
    args: &[&str],
    input: Option<&str>,
    stage: &'static str,
) -> Result<String> {
    run_bounded(runtime, tool, args, input, stage, DEADLINE)
}

pub(super) fn run_bounded(
    runtime: &Runtime,
    tool: &Path,
    args: &[&str],
    input: Option<&str>,
    stage: &'static str,
    deadline: Duration,
) -> Result<String> {
    runtime.block_on(invoke(tool, args, input, stage, deadline))
}

pub(super) async fn query(tool: &Path, stage: &'static str) -> Result<String> {
    invoke(tool, &[], None, stage, DEADLINE).await
}

async fn invoke(
    tool: &Path,
    args: &[&str],
    input: Option<&str>,
    stage: &'static str,
    deadline: Duration,
) -> Result<String> {
    let mut child = Command::new(tool)
        .args(args)
        .env_clear()
        .env("PATH", "/system/bin:/system/xbin:/vendor/bin")
        .env("ANDROID_ROOT", "/system")
        .env("ANDROID_DATA", "/data")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| stage)?;
    match tokio::time::timeout(deadline, exchange(&mut child, input)).await {
        Ok(Ok(output)) => Ok(output),
        _ => {
            let _ = child.kill().await;
            Err(stage)
        }
    }
}

async fn exchange(child: &mut Child, input: Option<&str>) -> std::io::Result<String> {
    let stdin = child.stdin.take();
    let Some(stdout) = child.stdout.take() else {
        return Err(std::io::Error::other("missing command output"));
    };
    let write = async {
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            stdin.write_all(input.as_bytes()).await?;
        }
        Ok(())
    };
    let read = async {
        let mut output = String::new();
        stdout.take(LIMIT + 1).read_to_string(&mut output).await?;
        if output.len() as u64 > LIMIT {
            return Err(std::io::Error::other("command output too large"));
        }
        Ok(output)
    };
    let ((), output, status) = tokio::try_join!(write, read, child.wait())?;
    if status.success() {
        Ok(output)
    } else {
        Err(std::io::Error::other("command failed"))
    }
}
