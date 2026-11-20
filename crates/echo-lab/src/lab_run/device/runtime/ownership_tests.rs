//! T3: real confined Linux filesystem shell, controlled adb and process observations.
#![cfg(unix)]
use super::super::{IDENTITY, Launcher, Remote, Shell};
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::{MetadataExt, symlink},
    path::PathBuf,
    process::Command,
    rc::Rc,
};

#[derive(Default)]
struct State {
    fail: String,
    lost_ack: bool,
    calls: Vec<Vec<String>>,
}

struct Filesystem(Rc<RefCell<State>>);

impl Shell for Filesystem {
    fn adb(&self, args: &[String]) -> Result<Vec<u8>, String> {
        let mut state = self.0.borrow_mut();
        state.calls.push(args.to_vec());
        let command = args.last().unwrap();
        if !state.fail.is_empty() && command.contains(&state.fail) && !state.lost_ack {
            return Err("injected failure".into());
        }
        match args[0].as_str() {
            "push" => {
                fs::copy(&args[1], &args[2]).map_err(|e| e.to_string())?;
                Ok(vec![])
            }
            "shell" if command.starts_with("chown ") => Ok(vec![]),
            "shell" if command.contains("stat -c %u:%g:%a") => Ok(b"9999:9999:600".to_vec()),
            "shell" => {
                let command = command.strip_prefix(&format!("{IDENTITY} ")).unwrap_or(command);
                let output = Command::new("sh").args(["-c", command]).output().unwrap();
                if !output.status.success()
                    || (!state.fail.is_empty() && state.lost_ack && command.contains(&state.fail))
                {
                    Err("shell failed or acknowledgement lost".into())
                } else {
                    Ok(output.stdout)
                }
            }
            _ => Err("unsupported fake operation".into()),
        }
    }
}

struct Fixture {
    root: tempfile::TempDir,
    state: Rc<RefCell<State>>,
    remote: Remote,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::write(private.join("bridge.toml"), b"installed sentinel\0\n").unwrap();
        fs::write(private.join("trace.jsonl"), b"installed trace\n").unwrap();
        fs::write(root.path().join("local"), b"bench config\n").unwrap();
        let launcher = Launcher {
            private_dir: private.to_str().unwrap().into(),
            config: private.join("bench.toml").to_str().unwrap().into(),
            trace: private.join("bench.jsonl").to_str().unwrap().into(),
            hassmic_log: root.path().join("hassmic.log").to_str().unwrap().into(),
            process: "echo-bridge".into(),
            run_as: String::new(),
            binary: Some("/bin/false".into()),
            start: None,
            stop: None,
        };
        let state = Rc::new(RefCell::new(State::default()));
        let remote = Remote::new(launcher, Box::new(Filesystem(state.clone())));
        Self { root, state, remote }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join("private").join(name)
    }

    fn stage(&mut self) -> Result<(), String> {
        self.remote.stage(&self.root.path().join("local"))
    }

    fn preserved(&self) {
        assert_eq!(fs::read(self.path("bridge.toml")).unwrap(), b"installed sentinel\0\n");
        assert_eq!(fs::read(self.path("trace.jsonl")).unwrap(), b"installed trace\n");
    }
}

#[test]
fn t3_on_device_exclusive_links_refuse_files_empty_files_directories_and_symlinks() {
    for name in [
        "bench.toml",
        "bench.jsonl",
        "lab-stdin",
        "lab-holder.pid",
        "lab-stdout.log",
        "lab-stderr.log",
    ] {
        for kind in 0..4 {
            let mut fixture = Fixture::new();
            let path = fixture.path(name);
            match kind {
                0 => fs::write(&path, b"foreign").unwrap(),
                1 => fs::write(&path, b"").unwrap(),
                2 => fs::create_dir(&path).unwrap(),
                _ => symlink(fixture.path("bridge.toml"), &path).unwrap(),
            }
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(fixture.stage().unwrap_err().contains("collision"));
            assert!(fixture.remote.release().is_err());
            assert_eq!(fs::symlink_metadata(&path).unwrap().ino(), metadata.ino());
            assert_eq!(fs::symlink_metadata(&path).unwrap().mode(), metadata.mode());
            if kind < 2 {
                assert_eq!(
                    fs::read(&path).unwrap(),
                    if kind == 0 { b"foreign".as_slice() } else { b"" }
                );
            }
            fixture.preserved();
        }
    }
}

#[test]
fn t3_on_device_two_owners_and_lost_ack_never_borrow_cleanup_authority() {
    let mut fixture = Fixture::new();
    fixture.stage().unwrap();
    let state = Rc::new(RefCell::new(State::default()));
    let mut loser = Remote::new(fixture.remote.launcher.clone(), Box::new(Filesystem(state)));
    assert!(loser.stage(&fixture.root.path().join("local")).is_err());
    assert!(loser.release().unwrap_err().contains("uncertain"));
    assert_eq!(fs::read(fixture.path("bench.toml")).unwrap(), b"bench config\n");
    fixture.remote.release().unwrap();
    fixture.preserved();

    for command in ["mkdir -m", "ln -T"] {
        let mut fixture = Fixture::new();
        fixture.state.borrow_mut().fail = command.into();
        fixture.state.borrow_mut().lost_ack = true;
        assert!(fixture.stage().is_err());
        fixture.state.borrow_mut().fail.clear();
        let result = fixture.remote.release();
        assert_eq!(result.is_err(), command == "mkdir -m");
        assert!(!fixture.path("bench.toml").exists());
        fixture.preserved();
    }
}

#[test]
fn t3_on_device_release_is_once_only_and_refuses_a_foreign_replacement() {
    for replace_before in [false, true] {
        let mut fixture = Fixture::new();
        fixture.stage().unwrap();
        assert_eq!(fs::metadata(fixture.path("bench.toml")).unwrap().mode() & 0o777, 0o600);
        let path = fixture.path("bench.toml");
        if replace_before {
            fs::remove_file(&path).unwrap();
            fs::write(&path, b"replacement").unwrap();
            assert!(fixture.remote.check_owned().is_err());
            assert!(fixture.remote.release().is_err());
            assert!(!fixture.path("bench.jsonl").exists());
        } else {
            fixture.remote.release().unwrap();
            fs::write(&path, b"replacement").unwrap();
        }
        fixture.remote.release().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"replacement");
        fixture.preserved();
    }
}

#[test]
fn t3_on_device_partial_acquisition_does_not_clean_a_replaced_owner_directory() {
    let mut fixture = Fixture::new();
    fixture.state.borrow_mut().fail = "push".into();
    // Fail after mkdir/identity by making the local input unavailable.
    fs::remove_file(fixture.root.path().join("local")).unwrap();
    assert!(fixture.stage().is_err());
    let directory = fixture.path("bench.toml.lab-owner");
    fs::rename(&directory, fixture.path("original-owner")).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("config"), b"foreign source").unwrap();
    fixture.state.borrow_mut().fail.clear();
    assert!(fixture.remote.release().unwrap_err().contains("directory replaced"));
    assert_eq!(fs::read(directory.join("config")).unwrap(), b"foreign source");
    fixture.preserved();
}

#[test]
fn t3_on_device_partial_acquisition_and_cleanup_errors_attempt_independent_entries() {
    for failure in ["chown ", "chmod ", "mkfifo ", "lab-stdout.log"] {
        let mut fixture = Fixture::new();
        fixture.state.borrow_mut().fail = failure.into();
        assert!(fixture.stage().is_err());
        fixture.state.borrow_mut().fail.clear();
        fixture.remote.release().unwrap();
        assert_eq!(fs::read_dir(fixture.path(".")).unwrap().count(), 2);
        fixture.preserved();
    }
    let mut fixture = Fixture::new();
    fixture.stage().unwrap();
    fixture.state.borrow_mut().fail = "rm -f".into();
    assert!(fixture.remote.release().unwrap_err().contains("residue retained"));
    assert_eq!(
        fixture
            .state
            .borrow()
            .calls
            .iter()
            .filter(|args| args.last().unwrap().contains("rm -f"))
            .count(),
        6
    );
    fixture.preserved();
}
