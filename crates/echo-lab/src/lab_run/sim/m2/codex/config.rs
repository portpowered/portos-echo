use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::process::Command;

/// A private external fixture owner, never a repository credential directory.
pub(in crate::lab_run::sim) struct Root(pub PathBuf);
impl Root {
    pub fn new() -> Result<Self, String> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "m2: private parent unavailable")?
            .join(format!(
                "echo-m2-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder.create(&path).map_err(|_| "m2: cannot create private root")?;
        }
        #[cfg(not(unix))]
        builder.create(&path).map_err(|_| "m2: cannot create private root")?;
        let root = Self(path);
        #[cfg(windows)]
        private_parent(&root.0)?;
        Ok(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn configure(
    command: &mut Command,
    root: &Path,
    port: u16,
    origin: &str,
    trace: &Path,
) -> Result<(), String> {
    let template = include_str!("../../../../../../../examples/bridge.example.toml");
    let start = template
        .find("[[mcp.server]]")
        .ok_or("m2: missing example MCP section")?;
    let end = start + template[start..].find("[screen]").ok_or("m2: missing example screen")?;
    let text = format!("{}[mcp]\nserver = []\n\n{}", &template[..start], &template[end..])
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("wyoming_port = 16700", &format!("wyoming_port = {port}"))
        .replace("mode = \"chatgpt\"", "mode = \"api_key\"")
        .replace("credentials/live.json", "key")
        .replace("voice = \"default\"\n", "")
        .replace("0.0.0.0:16900", "127.0.0.1:0") // lint-rules: allow-literal inert example listener replaced with loopback
        .replace("credentials/screen-pairing.txt", "pairing");
    for (name, contents) in [
        (
            "bridge.toml",
            format!("{text}\n[codex]\nhome = '{}'\n", root.join("home").display()),
        ),
        ("key", "dummy-local-only\n".into()),
        ("pairing", "synthetic-local-screen".into()),
    ] {
        std::fs::write(root.join(name), contents).map_err(|_| "m2: cannot write private config")?;
    }
    command.current_dir(root).args([
        "--config",
        "bridge.toml",
        "--health-listen",
        "127.0.0.1:0",
        "--live-loopback",
        origin,
        "--model-loopback",
        origin,
        "--sim-clock",
        "--trace",
    ]);
    command.arg(trace);
    Ok(())
}

#[cfg(windows)]
fn private_parent(path: &Path) -> Result<(), String> {
    let script = r"
$ErrorActionPreference = 'Stop'
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$acl = New-Object System.Security.AccessControl.DirectorySecurity
$acl.SetAccessRuleProtection($true, $false)
foreach ($sid in @($user.Value, 'S-1-5-18', 'S-1-5-32-544')) {
    $identity = New-Object System.Security.Principal.SecurityIdentifier $sid
    $rule = New-Object System.Security.AccessControl.FileSystemAccessRule(
        $identity, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    $acl.AddAccessRule($rule)
}
[System.IO.Directory]::SetAccessControl($env:ECHO_LAB_PRIVATE_PARENT, $acl)
";
    let mut command = std::process::Command::new("powershell.exe");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().eq_ignore_ascii_case("PSModulePath") {
            command.env_remove(key);
        }
    }
    let status = command
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
        .env("ECHO_LAB_PRIVATE_PARENT", path)
        .status()
        .map_err(|_| "m2: private ACL unavailable")?;
    if status.success() {
        Ok(())
    } else {
        Err("m2: private ACL unavailable".into())
    }
}
