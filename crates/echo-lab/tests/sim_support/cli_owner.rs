//! Private adapters retain the selected executable and collect its real child.
use std::io::Read;
use std::path::{Path, PathBuf};

fn install(source: &Path, target: &Path) {
    if !target.exists() && std::fs::hard_link(source, target).is_err() {
        std::fs::copy(source, target).unwrap();
    }
}

pub fn prepare(root: &Path) {
    let directory = root.join("owners");
    std::fs::create_dir(&directory).unwrap();
    let cli = Path::new(env!("CARGO_BIN_EXE_echo-lab"));
    install(cli, &directory.join(cli.file_name().unwrap()));
    // M7 discovers these real prebuilt peers beside the relocated CLI.
    for name in ["echo-muse-daemon", "echo-muse-report", "echo-muse-fake-gadget"] {
        let original = cli.with_file_name(name);
        if original.is_file() {
            install(&original, &directory.join(name));
        }
    }
    for name in ["echo-bridge", "echo-lab-sim-script", "echo-lab-sim-bridge"] {
        let original = cli.with_file_name(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.into()
        });
        if original.is_file() {
            selected(root, original.to_str().unwrap());
        }
    }
}

pub fn cli(root: &Path) -> PathBuf {
    root.join("owners")
        .join(Path::new(env!("CARGO_BIN_EXE_echo-lab")).file_name().unwrap())
}

pub fn selected(root: &Path, original: &str) -> String {
    let path = Path::new(original);
    let mut bytes = [0; 4];
    if std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_err()
    {
        return original.into();
    }
    // Preserve missing/invalid-executable tests at the delivered spawn boundary.
    if !(bytes.starts_with(b"MZ") || bytes.starts_with(b"\x7fELF")) {
        std::fs::write(root.join("owners/unlaunchable"), original).unwrap();
        return original.into();
    }
    let target = root.join("owners").join(path.file_name().unwrap());
    install(Path::new(env!("CARGO_BIN_EXE_echo-lab-sim-script")), &target);
    let selection = target.with_extension("target");
    if !selection.exists() {
        std::fs::write(selection, original).unwrap();
    }
    target.to_str().unwrap().into()
}

pub fn assert_closed(root: &Path) {
    let directory = root.join("owners");
    let mut count = 0;
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "started") {
            let bytes = std::fs::read(path.with_extension("collected"));
            assert!(bytes.is_ok(), "bridge listener owner did not complete");
            let receipt: serde_json::Value = serde_json::from_slice(&bytes.unwrap()).unwrap();
            assert_eq!(receipt["owner_completed"], true, "bridge listener owner survived");
            count += 1;
        }
    }
    assert!(
        count > 0 || directory.join("unlaunchable").is_file(),
        "bridge owner observation never started"
    );
}
