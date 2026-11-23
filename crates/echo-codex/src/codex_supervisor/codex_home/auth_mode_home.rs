use super::*;
use crate::codex_supervisor::{Supervisor, SupervisorConfig};
use echo_core_api::{Clock, SystemClock};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

const CHATGPT_AUTH: &[u8] = b"synthetic ChatGPT bytes\n";

struct Install {
    root: PathBuf,
    binary: PathBuf,
}

impl Install {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let nonce = SystemClock.now().wall.duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        let root = std::env::temp_dir().join(format!("echo-mode-{}-{nonce}", std::process::id()));
        let binary = std::env::current_exe()?;
        create_home(&root)?;
        let install = Self { root, binary };
        drop(install.take(HomeMode::Single)?);
        fs::write(install.base().join("auth.json"), CHATGPT_AUTH)?;
        Ok(install)
    }

    fn base(&self) -> PathBuf {
        self.root.join("custom.home")
    }

    fn api(&self) -> PathBuf {
        self.root.join("custom.home-api-key")
    }

    fn take(&self, mode: HomeMode) -> Result<Supervisor, ProcessError> {
        self.take_at(self.base(), mode)
    }

    fn take_at(&self, base: PathBuf, mode: HomeMode) -> Result<Supervisor, ProcessError> {
        let mut config = SupervisorConfig::new(self.binary.clone(), base);
        config.home_mode = mode;
        Supervisor::new(config)
    }

    fn assert_chatgpt_preserved(&self) -> std::io::Result<()> {
        assert!(fs::read(self.base().join("auth.json"))? == CHATGPT_AUTH);
        Ok(())
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn selects_custom_sibling_and_cleans_only_owned_auth() -> Result<(), Box<dyn std::error::Error>> {
    let install = Install::new()?;
    drop(install.take(HomeMode::Chatgpt).unwrap());
    assert!(!install.api().exists());
    let owner = install.take(HomeMode::ApiKey).unwrap();
    assert_eq!(owner.home.path, install.api());
    assert_eq!(owner.home.cwd(), install.api().join("work"));
    assert_eq!(owner.home.path.metadata().unwrap().permissions().mode() & 0o777, 0o700);
    drop(owner);
    assert!(matches!(
        Supervisor::new(SupervisorConfig::new(std::env::current_exe().unwrap(), install.api())),
        Err(ProcessError::PrivateHome)
    ));
    fs::write(install.api().join("auth.json"), b"synthetic API auth").unwrap();
    fs::write(install.api().join("cache"), b"cached models").unwrap();
    for _ in 0..2 {
        let owner = install.take(HomeMode::Chatgpt).unwrap();
        assert_eq!(owner.home.path, install.base());
        assert!(!install.api().join("auth.json").exists());
        assert_eq!(fs::read(install.api().join("cache")).unwrap(), b"cached models");
        install.assert_chatgpt_preserved()?;
    }
    Ok(())
}

#[test]
fn chatgpt_uses_existing_home_when_sibling_name_exceeds_the_filesystem_limit() -> Result<(), Box<dyn std::error::Error>>
{
    let install = Install::new()?;
    let base = install.root.join("h".repeat(248));
    drop(install.take_at(base.clone(), HomeMode::Single)?);
    let auth = base.join("auth.json");
    fs::write(&auth, CHATGPT_AUTH)?;
    let sibling = api_path(&base)?;
    assert_eq!(
        fs::symlink_metadata(sibling).unwrap_err().kind(),
        ErrorKind::InvalidFilename
    );
    let chatgpt = install.take_at(base.clone(), HomeMode::Chatgpt)?;
    assert_eq!(chatgpt.home.path, base);
    drop(chatgpt);
    assert!(matches!(
        install.take_at(base, HomeMode::ApiKey),
        Err(ProcessError::PrivateHome)
    ));
    assert!(fs::read(auth)? == CHATGPT_AUTH);
    Ok(())
}

#[test]
fn rejects_unowned_redirected_and_public_siblings() -> Result<(), Box<dyn std::error::Error>> {
    let install = Install::new()?;
    let sibling = install.api();
    symlink(install.base(), &sibling).unwrap();
    assert_rejected_in_both_modes(&install)?;
    fs::remove_file(&sibling).unwrap();
    create_home(&sibling).unwrap();
    fs::write(sibling.join("auth.json"), b"unowned file").unwrap();
    assert_rejected_in_both_modes(&install)?;
    assert_eq!(fs::read(sibling.join("auth.json")).unwrap(), b"unowned file");
    fs::remove_dir_all(&sibling).unwrap();
    drop(install.take(HomeMode::ApiKey).unwrap());
    fs::set_permissions(&sibling, fs::Permissions::from_mode(0o755)).unwrap();
    assert_rejected_in_both_modes(&install)?;
    Ok(())
}

#[test]
fn rejects_unsafe_auth_entries_and_releases_leases_after_failure() -> Result<(), Box<dyn std::error::Error>> {
    let install = Install::new()?;
    drop(install.take(HomeMode::ApiKey).unwrap());
    let original = install.base().join("auth.json");
    let api_auth = install.api().join("auth.json");
    symlink(&original, &api_auth).unwrap();
    assert_rejected_in_both_modes(&install)?;
    assert_eq!(fs::read_link(&api_auth).unwrap(), original);
    fs::remove_file(&api_auth).unwrap();
    fs::hard_link(&original, &api_auth).unwrap();
    assert_rejected_in_both_modes(&install)?;
    assert_eq!(fs::metadata(&api_auth).unwrap().nlink(), 2);
    fs::remove_file(&api_auth).unwrap();
    create_home(&api_auth).unwrap();
    fs::write(api_auth.join("keep"), b"keep").unwrap();
    assert_rejected_in_both_modes(&install)?;
    assert_eq!(fs::read(api_auth.join("keep")).unwrap(), b"keep");
    fs::remove_dir_all(api_auth).unwrap();
    drop(install.take(HomeMode::Chatgpt).unwrap());
    Ok(())
}

#[test]
fn holds_installation_and_api_leases_across_modes() -> Result<(), Box<dyn std::error::Error>> {
    let install = Install::new()?;
    let owner = install.take(HomeMode::ApiKey).unwrap();
    let marker = OpenOptions::new()
        .read(true)
        .write(true)
        .open(install.api().join(API_MARKER))
        .unwrap();
    let api_auth = install.api().join("auth.json");
    fs::write(&api_auth, b"synthetic API auth").unwrap();
    assert_rejected_in_both_modes(&install)?;
    assert!(matches!(marker.try_lock(), Err(fs::TryLockError::WouldBlock)));
    drop(owner);

    marker.try_lock().unwrap();
    assert_rejected_in_both_modes(&install)?;
    assert_eq!(fs::read(&api_auth).unwrap(), b"synthetic API auth");
    marker.unlock().unwrap();
    let owner = install.take(HomeMode::Chatgpt).unwrap();
    assert!(!api_auth.exists());
    assert_rejected_in_both_modes(&install)?;
    assert!(matches!(marker.try_lock(), Err(fs::TryLockError::WouldBlock)));
    drop(owner);
    marker.try_lock().unwrap();
    Ok(())
}

fn assert_rejected_in_both_modes(install: &Install) -> std::io::Result<()> {
    for mode in [HomeMode::Chatgpt, HomeMode::ApiKey] {
        assert!(matches!(install.take(mode), Err(ProcessError::PrivateHome)));
    }
    install.assert_chatgpt_preserved()
}
