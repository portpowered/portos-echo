use super::*;
use echo_codex::codex_supervisor::{Supervisor, SupervisorConfig};

pub(super) struct Fixture {
    pub directory: tempfile::TempDir,
    pub home: PathBuf,
    pub binary: PathBuf,
    pub screen: std::net::SocketAddr,
}

impl Fixture {
    pub fn new(mode: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        private_parent(directory.path());
        let binary = std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(binary.status.success());
        let binary = PathBuf::from(String::from_utf8(binary.stdout).unwrap().trim());
        let home = directory.path().join("home");
        let owner = Supervisor::new(SupervisorConfig::new(binary.clone(), home.clone())).unwrap();
        std::fs::write(home.join("work/app-server"), include_str!("peer.py")).unwrap();
        std::fs::write(home.join("mode"), mode).unwrap();
        drop(owner);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let screen = listener.local_addr().unwrap();
        drop(listener);
        let fixture = Self {
            directory,
            home,
            binary,
            screen,
        };
        fixture.configure(mode);
        fixture
    }

    fn configure(&self, mode: &str) {
        let path = self.directory.path();
        bridge_wire_auth::config(path, mode, "");
        std::fs::write(path.join("screen-token"), "synthetic-screen-pair").unwrap();
        let text = std::fs::read_to_string(path.join("bridge.toml"))
            .unwrap()
            .replace(
                "credentials/screen-pairing.txt",
                &path.join("screen-token").display().to_string().replace('\\', "/"),
            )
            .replace("127.0.0.1:0", &self.screen.to_string());
        std::fs::write(
            path.join("bridge.toml"),
            format!(
                "{text}\n[codex]\nhome = '{}'\nbinary = '{}'\n",
                self.home.display(),
                self.binary.display()
            ),
        )
        .unwrap();
    }

    pub fn owner(&self) -> Supervisor {
        Supervisor::new(SupervisorConfig::new(self.binary.clone(), self.home.clone())).unwrap()
    }

    pub async fn invoke(&self, args: &[&str], code: i32, stdout: &str, stderr: &str) {
        let output = tokio::time::timeout(
            OBSERVATION_TIMEOUT,
            bridge_wire_auth::command(self.directory.path(), args)
                .env("OPENAI_API_KEY", "synthetic-inherited-key")
                .env("OPENAI_ACCESS_TOKEN", "synthetic-inherited-token")
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert_eq!(String::from_utf8(output.stdout).unwrap(), stdout);
        assert_eq!(String::from_utf8(output.stderr).unwrap(), stderr);
    }

    pub async fn control(&self) -> tokio::net::TcpStream {
        let port = tokio::time::timeout(OBSERVATION_TIMEOUT, async {
            loop {
                if let Ok(text) = std::fs::read_to_string(self.home.join("peer-port"))
                    && let Ok(port) = text.parse::<u16>()
                {
                    break port;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap()
    }
}

#[cfg(windows)]
fn private_parent(path: &Path) {
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
Set-Acl -LiteralPath $env:ECHO_OPERATOR_TEST_PARENT -AclObject $acl
";
    assert!(
        std::process::Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
            .env_remove("PSModulePath")
            .env("ECHO_OPERATOR_TEST_PARENT", path)
            .status()
            .unwrap()
            .success()
    );
}
