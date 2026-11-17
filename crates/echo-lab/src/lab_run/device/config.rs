use std::{io::Write, path::Path};

use super::super::{run_command, run_options::Options};

pub fn load(options: &Options) -> Result<Option<String>, String> {
    let Some(path) = &options.bridge_config else {
        return Ok(None);
    };
    let config = echo_config::BridgeConfig::load(path)
        .map_err(|_| "device: invalid or unsafe nominated bridge configuration")?;
    if config.echo.host != "127.0.0.1" {
        return Err("device: echo.host must be 127.0.0.1; forward the Echo's Wyoming port with adb forward".into());
    }
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
    let contents = value["toml"].as_str().ok_or("device: missing configuration result")?;
    echo_config::BridgeConfig::from_toml_str(contents).map_err(|_| "device: invalid projected configuration")?;
    Ok(Some(contents.to_owned()))
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
        if name == "bridge.toml" {
            echo_config::BridgeConfig::load(directory.join(name))
                .map_err(|_| "device: invalid staged configuration")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_staging_revalidates_before_writing_synthetic_inputs() {
        let root = tempfile::tempdir().unwrap();
        let contents = include_str!("../../../../../examples/bridge.example.toml")
            .replace("max_tool_calls = 8", "max_tool_calls = 0");
        assert_eq!(
            stage(root.path(), &contents),
            Err("device: invalid staged configuration".into())
        );
        let directory = root.path().join("configured.private");
        assert_eq!(
            std::fs::read_to_string(directory.join("bridge.toml")).unwrap(),
            contents
        );
        assert!(!directory.join("credential").exists());
        assert!(!directory.join("pairing").exists());
    }

    #[test]
    fn device_staging_preserves_valid_configuration_without_reading_credential_paths() {
        let root = tempfile::tempdir().unwrap();
        let contents = include_str!("../../../../../examples/bridge.example.toml");
        stage(root.path(), contents).unwrap();
        let directory = root.path().join("configured.private");
        assert_eq!(
            std::fs::read_to_string(directory.join("bridge.toml")).unwrap(),
            contents
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("credential")).unwrap(),
            "synthetic-local-api"
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("pairing")).unwrap(),
            "synthetic-local-screen"
        );
        assert!(!root.path().join("credentials").exists());
    }
}
