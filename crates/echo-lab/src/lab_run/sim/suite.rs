use std::io::Write;
use std::path::{Path, PathBuf};

pub fn execute(args: &[String], output: &mut impl Write) -> Result<(), String> {
    let (extras, root) = options(args)?;
    let (bridge, paths) = discover()?;
    run_suite(&bridge, paths, &extras, root.as_deref(), output)
}

fn discover() -> Result<(PathBuf, Vec<PathBuf>), String> {
    let bridge = bundled_bridge()?;
    let mut paths = std::fs::read_dir("tests/scenarios")
        .map_err(|_| "sim suite: cannot discover scenarios")?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "sim suite: cannot read scenario entry")?;
    paths.retain(|path| path.extension().is_some_and(|e| e == "yaml"));
    paths.sort();
    Ok((bridge, paths))
}

pub fn bundled_bridge() -> Result<PathBuf, String> {
    prebuilt("echo-lab-sim-bridge")
}

pub fn default_bridge(scenario: &serde_json::Value) -> Result<PathBuf, String> {
    if scenario["milestone"] == "M5" {
        return production_bridge();
    }
    prebuilt(
        if matches!(scenario["milestone"].as_str(), Some("M1" | "M2"))
            || matches!(
                scenario["id"].as_str(),
                Some("m4-spoof-tool" | "m6-emotion" | "m7-muse")
            )
        {
            "echo-bridge"
        } else {
            "echo-lab-sim-bridge"
        },
    )
}

fn prebuilt(name: &str) -> Result<PathBuf, String> {
    let bridge = std::env::current_exe()
        .map_err(|_| "sim: cannot locate prebuilt bridge")?
        .with_file_name(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.into()
        });
    if !bridge.is_file() {
        let packages = if name == "echo-bridge" {
            "-p echo-lab -p echo-bridge"
        } else {
            "-p echo-lab"
        };
        return Err(format!(
            "sim: prebuilt bridge missing at {}; run cargo build {packages} --bins --locked",
            bridge.display()
        ));
    }
    Ok(bridge)
}

pub fn production_bridge() -> Result<PathBuf, String> {
    let bridge = std::env::current_exe()
        .map_err(|_| "M5: cannot locate production bridge")?
        .with_file_name(if cfg!(windows) {
            "echo-bridge.exe"
        } else {
            "echo-bridge"
        });
    if !bridge.is_file() {
        return Err("M5: prebuilt echo-bridge missing; run cargo build --locked -p echo-bridge --bins".into());
    }
    Ok(bridge)
}

fn run_suite(
    bridge: &Path,
    paths: Vec<PathBuf>,
    extras: &[String],
    root: Option<&Path>,
    output: &mut impl Write,
) -> Result<(), String> {
    let mut passed = 0;
    let mut failed = 0;
    let unavailable = 0;
    for path in paths {
        let name = path.to_str().ok_or("sim suite: invalid scenario path")?;
        let scenario = crate::lab_validate::load(name)?;
        if !scenario["tiers"]
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|t| t == "T3"))
        {
            continue;
        }
        let mut command = scenario_command(bridge, name, &scenario)?;
        command.extend_from_slice(extras);
        if let Some(root) = &root {
            let milestone = scenario["milestone"].as_str().ok_or("sim suite: missing milestone")?;
            let parent = destination(root, milestone)?;
            command.extend([
                "--out".into(),
                parent
                    .join(format!("{}-{}", std::process::id(), passed + failed))
                    .to_string_lossy()
                    .into_owned(),
            ]);
        }
        let result = super::super::execute(&command, output);
        match result {
            Ok(()) => passed += 1,
            Err(error) => {
                failed += 1;
                writeln!(output, "{name}: fail ({error})").map_err(|_| "sim suite: stdout failed")?;
            }
        }
    }
    writeln!(
        output,
        "executed={} passed={passed} failed={failed} not_runnable={unavailable}",
        passed + failed
    )
    .map_err(|_| "sim suite: stdout failed")?;
    if failed != 0 || passed == 0 {
        return Err("sim suite: runnable failure or no executed T3 scenarios".into());
    }
    Ok(())
}

fn scenario_command(bridge: &Path, name: &str, scenario: &serde_json::Value) -> Result<Vec<String>, String> {
    let production;
    let selected = if matches!(scenario["milestone"].as_str(), Some("M1" | "M2" | "M5"))
        || matches!(
            scenario["id"].as_str(),
            Some("m4-spoof-tool" | "m6-emotion" | "m7-muse")
        ) {
        production = default_bridge(scenario)?;
        production.as_path()
    } else {
        bridge
    };
    Ok(vec![
        name.into(),
        "--driver".into(),
        "sim".into(),
        "--bridge".into(),
        selected.to_string_lossy().into_owned(),
    ])
}

fn options(args: &[String]) -> Result<(Vec<String>, Option<PathBuf>), String> {
    let mut extras = Vec::new();
    let mut root = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if !seen.insert(arg) {
            return Err("sim suite: duplicate option".into());
        }
        match arg.as_str() {
            "--all" => {}
            "--driver" if args.get(index + 1).is_some_and(|s| s == "sim") => {
                index += 1;
            }
            "--python" | "--cancel-file" => {
                index += 1;
                extras.extend([
                    arg.clone(),
                    args.get(index)
                        .filter(|s| !s.starts_with("--"))
                        .ok_or("sim suite: missing option value")?
                        .clone(),
                ]);
            }
            "--out" => {
                index += 1;
                let path = args.get(index).ok_or("sim suite: missing artifact root")?;
                let path = super::super::run_options::destination(Path::new(path))?;
                std::fs::create_dir(&path).map_err(|_| "sim suite: cannot own fresh artifact root")?;
                root = Some(path);
            }
            _ => return Err("sim suite: unknown option or missing value".into()),
        }
        index += 1;
    }
    if !seen.contains(&"--driver".to_owned()) {
        return Err("sim suite: requires --driver sim".into());
    }
    Ok((extras, root))
}

pub fn destination(root: &Path, milestone: &str) -> Result<PathBuf, String> {
    if milestone.is_empty()
        || !milestone
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err("sim suite: invalid milestone directory".into());
    }
    let parent = root.join("artifacts").join(milestone);
    std::fs::create_dir_all(&parent).map_err(|_| "sim suite: cannot create artifact tree")?;
    Ok(parent)
}

/// M4 consumes the delivered bridge; earlier scenarios retain their helper.
pub fn scenario_bridge(scenario: &serde_json::Value) -> Result<PathBuf, String> {
    if !matches!(scenario["id"].as_str(), Some("m4-spoof-tool" | "m6-emotion")) {
        return default_bridge(scenario);
    }
    let bridge = std::env::current_exe()
        .map_err(|_| "m4: cannot locate prebuilt bridge")?
        .with_file_name(if cfg!(windows) {
            "echo-bridge.exe"
        } else {
            "echo-bridge"
        });
    if !bridge.is_file() {
        return Err("m4: missing prebuilt echo-bridge; run cargo build -p echo-bridge --bins --locked".into());
    }
    Ok(bridge)
}
