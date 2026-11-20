//! A bounded native process boundary; never forward adb output to diagnostics.
use crate::{Failure, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

pub struct Adb {
    executable: PathBuf,
    serial: Option<OsString>,
}

impl Adb {
    pub fn discover(explicit: Option<&Path>) -> Result<Self> {
        let executable = if let Some(path) = explicit {
            path.is_file().then(|| path.to_path_buf())
        } else {
            candidates().into_iter().find(|path| path.is_file())
        }
        .ok_or(Failure("adb_required"))?;
        Ok(Self {
            executable,
            serial: None,
        })
    }

    pub async fn select(&mut self, requested: Option<&OsStr>) -> Result<()> {
        self.select_ready(requested).await?;
        self.shell("test -x /data/local/hassmic/runas")
            .await
            .map_err(|_| Failure("hassmic_required"))?;
        Ok(())
    }

    pub async fn select_ready(&mut self, requested: Option<&OsStr>) -> Result<()> {
        let devices = self.call(&[OsString::from("devices")]).await?;
        self.serial = Some(select_device(&devices, requested)?);
        Ok(())
    }

    pub async fn shell(&self, command: &str) -> Result<String> {
        self.call(&[OsString::from("shell"), OsString::from(command)]).await
    }

    pub async fn push(&self, local: &Path, remote: &str) -> Result<()> {
        self.call(&[
            OsString::from("push"),
            local.as_os_str().to_owned(),
            OsString::from(remote),
        ])
        .await?;
        Ok(())
    }

    async fn call(&self, arguments: &[OsString]) -> Result<String> {
        let (success, output) = self.execute(arguments, None).await?;
        if !success {
            return Err(Failure("adb_failed"));
        }
        Ok(output)
    }

    // Streaming avoids host command-line limits and never places a host path on Android.
    pub async fn script(&self, operation: &str) -> Result<(bool, String)> {
        self.execute(
            &[
                OsString::from("shell"),
                OsString::from(format!("sh -s -- --device {operation}")),
            ],
            Some(crate::DEVICE_SCRIPT.as_bytes()),
        )
        .await
    }

    async fn execute(&self, arguments: &[OsString], input: Option<&[u8]>) -> Result<(bool, String)> {
        let mut command = Command::new(&self.executable);
        if let Some(serial) = &self.serial {
            command.arg("-s").arg(serial);
        }
        let mut child = command
            .args(arguments)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| Failure("adb_failed"))?;
        let stdout = child.stdout.take().ok_or(Failure("adb_failed"))?;
        let stdin = child.stdin.take();
        let operation = async {
            let ((), output) = tokio::try_join!(write_input(stdin, input), read_output(stdout))?;
            let success = child.wait().await.map_err(|_| Failure("adb_failed"))?.success();
            Ok((success, output))
        };
        tokio::time::timeout(Duration::from_secs(600), operation)
            .await
            .map_err(|_| Failure("adb_timeout"))?
    }
}

async fn write_input(stdin: Option<tokio::process::ChildStdin>, input: Option<&[u8]>) -> Result<()> {
    if let (Some(mut stdin), Some(input)) = (stdin, input) {
        stdin.write_all(input).await.map_err(|_| Failure("adb_failed"))?;
        stdin.shutdown().await.map_err(|_| Failure("adb_failed"))?;
    }
    Ok(())
}

async fn read_output(stdout: tokio::process::ChildStdout) -> Result<String> {
    let mut bytes = Vec::new();
    stdout
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| Failure("adb_failed"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(Failure("adb_output_limit"));
    }
    String::from_utf8(bytes)
        .map(|text| text.replace("\r\n", "\n"))
        .map_err(|_| Failure("adb_failed"))
}

fn candidates() -> Vec<PathBuf> {
    let name = if cfg!(windows) { "adb.exe" } else { "adb" };
    let mut paths = std::env::var_os("PATH").map_or_else(Vec::new, |value| {
        std::env::split_paths(&value).map(|root| root.join(name)).collect()
    });
    for variable in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(root) = std::env::var_os(variable) {
            paths.push(PathBuf::from(root).join("platform-tools").join(name));
        }
    }
    if let Some(root) = std::env::var_os("LOCALAPPDATA") {
        paths.push(PathBuf::from(root).join("Android/Sdk/platform-tools").join(name));
    }
    if let Some(root) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(root).join("Android/Sdk/platform-tools").join(name));
    }
    paths
}

/// Device identities stay opaque and are used only as a native argv element.
pub fn select_device(output: &str, requested: Option<&OsStr>) -> Result<OsString> {
    let mut lines = output.lines();
    if lines.next() != Some("List of devices attached") {
        return Err(Failure("device_selection_failed"));
    }
    let devices: Vec<_> = lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .collect();
    let selected = if let Some(requested) = requested {
        let matches: Vec<_> = devices
            .iter()
            .filter(|parts| parts.first().is_some_and(|v| OsStr::new(v) == requested))
            .collect();
        if matches.len() != 1 {
            return Err(Failure("device_selection_failed"));
        }
        matches[0]
    } else {
        if devices.len() != 1 {
            return Err(Failure("exactly_one_device_required"));
        }
        &devices[0]
    };
    if selected.len() != 2 || selected[1] != "device" {
        return Err(Failure("device_not_ready"));
    }
    Ok(OsString::from(selected[0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_normalizes_native_adb_line_endings() -> Result<()> {
        for newline in ["\n", "\r\n"] {
            let output = format!("List of devices attached{newline}SYNTHETIC-DEVICE\tdevice{newline}{newline}");
            assert_eq!(select_device(&output, None)?, OsString::from("SYNTHETIC-DEVICE"));
        }
        Ok(())
    }

    #[test]
    fn selection_requires_one_ready_device_or_exact_selection() -> Result<()> {
        let output = "List of devices attached\nSYNTHETIC-A\tdevice\nSYNTHETIC-B\tdevice\n";
        assert_eq!(select_device(output, None), Err(Failure("exactly_one_device_required")));
        assert_eq!(
            select_device(output, Some(OsStr::new("SYNTHETIC-B")))?,
            OsString::from("SYNTHETIC-B")
        );
        assert_eq!(
            select_device(output, Some(OsStr::new("SYNTHETIC"))),
            Err(Failure("device_selection_failed"))
        );
        for state in ["offline", "unauthorized"] {
            assert_eq!(
                select_device(&format!("List of devices attached\nSYNTHETIC-A\t{state}\n"), None),
                Err(Failure("device_not_ready"))
            );
        }
        assert_eq!(
            select_device("List of devices attached\n", None),
            Err(Failure("exactly_one_device_required"))
        );
        Ok(())
    }
}
