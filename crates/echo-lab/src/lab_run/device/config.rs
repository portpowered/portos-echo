use std::{io::Write, path::Path};

use super::super::{run_command, run_options::Options};

pub fn load(options: &Options) -> Result<Option<String>, String> {
    let Some(path) = &options.bridge_config else {
        return Ok(None);
    };
    let bytes = run_command::controlled(
        Path::new(&options.python),
        &[
            "-c".into(),
            include_str!("config.py").into(),
            path.to_string_lossy().into_owned(),
        ],
        std::time::Instant::now() + std::time::Duration::from_secs(5),
        options.cancel.as_deref(),
    )
    .map_err(|_| "device: invalid or unsafe nominated bridge configuration")?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "device: invalid configuration result")?;
    value["toml"]
        .as_str()
        .map(|s| Some(s.to_owned()))
        .ok_or_else(|| "device: missing configuration result".into())
}

pub fn stage(root: &Path, contents: &str) -> Result<(), String> {
    let directory = root.join("configured.private");
    std::fs::create_dir(&directory).map_err(|_| "device: cannot create private configuration")?;
    for (name, contents) in [
        ("bridge.toml", contents),
        ("credential", "synthetic-local-api"),
        ("pairing", "synthetic-local-screen"),
    ] {
        let path = directory.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .and_then(|mut file| file.write_all(contents.as_bytes()))
            .map_err(|_| "device: cannot write private configuration")?;
    }
    Ok(())
}
