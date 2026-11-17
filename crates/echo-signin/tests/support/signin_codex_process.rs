//! Controlled stdio process: public account adapter, Supervisor and retained state.
use echo_codex::codex_supervisor::{ProcessError, Supervisor, SupervisorConfig};
use echo_config::LiveMode;
use echo_signin::{OperatorCommand, signin_codex::execute};
use std::{error::Error, fs, path::PathBuf, process::Command, time::Duration};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let parent = std::env::temp_dir().join(format!("echo-account-process-{}", std::process::id()));
        fs::create_dir(&parent)?;
        let fixture = Self(parent);
        #[cfg(windows)]
        fixture.make_private()?;
        Ok(fixture)
    }

    #[cfg(windows)]
    fn make_private(&self) -> Result {
        let script = r"
$ErrorActionPreference = 'Stop'
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$acl = New-Object System.Security.AccessControl.DirectorySecurity
$acl.SetOwner($user)
$acl.SetAccessRuleProtection($true, $false)
foreach ($sid in @($user.Value, 'S-1-5-18', 'S-1-5-32-544')) {
    $identity = New-Object System.Security.Principal.SecurityIdentifier $sid
    $rule = New-Object System.Security.AccessControl.FileSystemAccessRule(
        $identity, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    $acl.AddAccessRule($rule)
}
Set-Acl -LiteralPath $env:ECHO_ACCOUNT_TEST_PARENT -AclObject $acl
";
        let status = Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
            .env_remove("PSModulePath")
            .env("ECHO_ACCOUNT_TEST_PARENT", &self.0)
            .status()?;
        assert!(status.success());
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn python_binary() -> Result<PathBuf> {
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args(["-c", "import sys; print(sys.executable)"])
        .output()?;
    assert!(output.status.success());
    Ok(PathBuf::from(String::from_utf8(output.stdout)?.trim()))
}

#[tokio::test]
async fn codex_process_signin_shutdown_reopen_status_preserves_synthetic_auth() -> Result {
    let fixture = Fixture::new()?;
    let home = fixture.0.join("home");
    let binary = python_binary()?;
    let config = || SupervisorConfig::new(binary.clone(), home.clone());
    let mut supervisor = Supervisor::new(config())?;
    fs::write(home.join("work/app-server"), include_str!("codex_account_peer.py"))?;
    supervisor.start().await?;
    let mut output = Vec::new();
    let mut screen = String::new();
    let result = execute(
        supervisor.io()?,
        LiveMode::Chatgpt,
        OperatorCommand::Signin { browser: false },
        &mut output,
        |prompt| {
            screen = prompt.message();
            Ok(())
        },
        tokio::time::Instant::now() + Duration::from_secs(5),
    )
    .await;
    supervisor.shutdown().await?;
    assert_eq!(supervisor.pid(), None);
    assert_eq!(result, Ok(()));
    assert_eq!(screen, "code: TEST-CODE\nurl: http://127.0.0.1:12345/verify\n");
    assert_eq!(output, format!("{screen}signed in\n").as_bytes());
    assert!(matches!(Supervisor::new(config()), Err(ProcessError::PrivateHome)));
    drop(supervisor);
    let before = fs::read(home.join("auth.json"))?;
    let mut reopened = Supervisor::new(config())?;
    reopened.start().await?;
    let result = execute(
        reopened.io()?,
        LiveMode::Chatgpt,
        OperatorCommand::Status,
        &mut Vec::new(),
        |_| Ok(()),
        tokio::time::Instant::now() + Duration::from_secs(5),
    )
    .await;
    reopened.shutdown().await?;
    assert_eq!(reopened.pid(), None);
    assert_eq!(result, Ok(()));
    assert_eq!(fs::read(home.join("auth.json"))?, before);
    assert_eq!(before, b"{\"synthetic\":\"local-only\"}\n");
    Ok(())
}
