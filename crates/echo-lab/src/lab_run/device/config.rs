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
    use std::path::PathBuf;

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

    const REFUSAL: &str = "device: invalid or unsafe nominated bridge configuration";

    fn nomination() -> String {
        include_str!("../../../../../examples/bridge.example.toml")
            .lines()
            .map(|line| {
                if line.starts_with("host =") {
                    "host = \"127.0.0.1\""
                } else if line.starts_with("listen =") {
                    "listen = \"127.0.0.1:0\""
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn options(path: &Path) -> Options {
        Options {
            bridge_config: Some(path.to_owned()),
            python: "python3".into(),
            wake_clip: None,
            sim: false,
            bridge: None,
            scenarios: Vec::new(),
            port: PathBuf::default(),
            adb: PathBuf::default(),
            stt: PathBuf::default(),
            ring: String::new(),
            out: PathBuf::default(),
            keep_audio: false,
            speech: None,
            cancel: None,
            operator_confirm: None,
            allow_quiet_hours: false,
        }
    }

    // T3: real Python/filesystem, synthetic inputs only. The audit hook observes
    // opens and directory access independently of the sentinel equality check.
    fn observed_projection(path: &Path, home: &Path) -> serde_json::Value {
        let observer = r"
import contextlib, io, json, os, sys
code, config, home = sys.argv[1:]
home = os.path.normcase(os.path.abspath(home))
accesses = []
def observe(event, args):
    if event not in ('open', 'os.listdir', 'os.scandir', 'os.chdir'):
        return
    if args and isinstance(args[0], (str, bytes, os.PathLike)):
        path = os.path.normcase(os.path.abspath(os.fsdecode(args[0])))
        if path == home or path.startswith(home + os.sep):
            accesses.append(event)
sys.addaudithook(observe)
sys.argv = ['config.py', config]
stdout, stderr = io.StringIO(), io.StringIO()
status = 0
with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
    try:
        exec(compile(code, 'config.py', 'exec'), {'__name__': '__main__'})
    except SystemExit as error:
        status = error.code
print(json.dumps(dict(status=status, stdout=stdout.getvalue(),
                      stderr=stderr.getvalue(), accesses=accesses)))
";
        let bytes = run_command::controlled(
            Path::new(&options(path).python),
            &[
                "-c".into(),
                observer.into(),
                include_str!("config.py").into(),
                path.to_string_lossy().into_owned(),
                home.to_string_lossy().into_owned(),
            ],
            std::time::Instant::now() + std::time::Duration::from_secs(5),
            None,
        )
        .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn assert_refused(path: &Path, home: &Path, run: &Path) {
        let result = load(&options(path));
        if let Ok(Some(contents)) = &result {
            stage(run, contents).unwrap();
        }
        assert_eq!(result, Err(REFUSAL.into()));
        assert!(!run.join("configured.private").exists());
        let observed = observed_projection(path, home);
        assert_eq!(observed["status"], 1);
        assert_eq!(observed["stdout"], "");
        assert_eq!(observed["stderr"], format!("{REFUSAL}\n"));
        assert_eq!(observed["accesses"], serde_json::json!([]));
    }

    #[test]
    fn t3_device_config_refuses_codex_home_without_access_or_staging() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let sentinel = b"inert synthetic sentinel\n";
        let auth = home.path().join("auth.json");
        std::fs::write(&auth, sentinel).unwrap();
        let before: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let path = root.path().join("nomination.toml");
        let home_value = serde_json::to_string(&home.path().to_string_lossy()).unwrap();
        let contents = format!("{}\n[codex]\nhome = {home_value}\n", nomination());
        // Ensure this reaches the projector, rather than failing schema validation.
        echo_config::BridgeConfig::from_toml_str(&contents).unwrap();
        std::fs::write(&path, contents).unwrap();
        assert_refused(&path, home.path(), root.path());
        assert_eq!(std::fs::read(auth).unwrap(), sentinel);
        let after: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn t3_device_config_refuses_nonexistent_codex_home_and_mcp_state() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("absent-home");
        let value = serde_json::to_string(&home.to_string_lossy()).unwrap();
        let path = root.path().join("nomination.toml");
        for contents in [
            format!("{}\n[codex]\nhome = {value}\n", nomination()),
            nomination().replace("[[mcp.server]]", &format!("[mcp]\nstate_dir = {value}\n[[mcp.server]]")),
        ] {
            echo_config::BridgeConfig::from_toml_str(&contents).unwrap();
            std::fs::write(&path, contents).unwrap();
            assert_refused(&path, &home, root.path());
            assert!(!home.exists());
        }
    }

    fn assert_safe_projection(contents: &str, instructions: &str) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("nomination.toml");
        std::fs::write(&path, contents).unwrap();
        let projected = load(&options(&path)).unwrap().unwrap();
        let actual = echo_config::BridgeConfig::from_toml_str(&projected).unwrap();
        let mut expected = echo_config::BridgeConfig::from_toml_str(contents).unwrap();
        expected.live.mode = echo_config::LiveMode::ApiKey;
        expected.live.credential_file = "credential".into();
        expected.live.voice = Some("alloy".into());
        expected.live.session_idle_seconds = 0;
        expected.screen.listen = "127.0.0.1:0".parse().unwrap();
        expected.screen.pairing_token_file = "pairing".into();
        expected.audit.log_dir = "audit".into();
        assert_eq!(actual, expected);
        assert_eq!(actual.codex.home, None);
        assert_eq!(actual.codex.instructions, instructions);
        stage(root.path(), &projected).unwrap();
        let directory = root.path().join("configured.private");
        assert_eq!(
            std::fs::read_to_string(directory.join("bridge.toml")).unwrap(),
            projected
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("credential")).unwrap(),
            "synthetic-local-api"
        );
        assert_eq!(
            std::fs::read_to_string(directory.join("pairing")).unwrap(),
            "synthetic-local-screen"
        );
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 3);
        assert!(!root.path().join("credentials").exists());
    }

    #[test]
    fn t3_device_config_without_codex_preserves_safe_projection_and_stage() {
        assert_safe_projection(&nomination(), "Client audio bridge");
    }

    #[test]
    fn t3_device_config_preserves_home_free_codex_settings() {
        let contents = format!(
            "{}\n[codex]\ninstructions = \"Synthetic bridge instructions\"\n",
            nomination()
        );
        assert_safe_projection(&contents, "Synthetic bridge instructions");
    }
}
