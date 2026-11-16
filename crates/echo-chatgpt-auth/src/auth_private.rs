use crate::AuthError;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) struct Directory(pub(crate) PathBuf);
impl Directory {
    pub(crate) fn new(parent: &Path) -> Result<Self, AuthError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..100 {
            let path = parent.join(format!(
                ".echo-auth-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let builder = fs::DirBuilder::new();
            #[cfg(unix)]
            let mut builder = builder;
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => {
                    let directory = Self(path);
                    #[cfg(windows)]
                    protect(&directory.0)?;
                    return Ok(directory);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(AuthError::Storage),
            }
        }
        Err(AuthError::Storage)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[cfg(windows)]
fn protect(path: &Path) -> Result<(), AuthError> {
    use std::os::windows::process::CommandExt;
    // An empty directory is protected before any credential bytes exist.
    let script = r"$ErrorActionPreference='Stop';
$p=$env:ECHO_AUTH_PRIVATE_DIR;
$sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User;
$acl=[System.Security.AccessControl.DirectorySecurity]::new();
$acl.SetOwner($sid);
$acl.SetAccessRuleProtection($true,$false);
$rule=[System.Security.AccessControl.FileSystemAccessRule]::new($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow');
$acl.AddAccessRule($rule);
[System.IO.Directory]::SetAccessControl($p,$acl);";
    let result = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("ECHO_AUTH_PRIVATE_DIR", path)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|_| AuthError::Storage)?;
    if result.status.success() {
        Ok(())
    } else {
        Err(AuthError::Storage)
    }
}
