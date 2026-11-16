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
    let bridge = std::env::current_exe()
        .map_err(|_| "sim: cannot locate prebuilt bridge; run cargo build -p echo-lab --bins --locked")?
        .with_file_name(if cfg!(windows) {
            "echo-lab-sim-bridge.exe"
        } else {
            "echo-lab-sim-bridge"
        });
    if !bridge.is_file() {
        return Err(format!(
            "sim: bundled bridge missing at {}; run cargo build -p echo-lab --bins --locked",
            bridge.display()
        ));
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
    let mut unavailable = 0;
    for path in paths {
        let name = path.to_str().ok_or("sim suite: invalid scenario path")?;
        let scenario = crate::lab_validate::load(name)?;
        if !scenario["tiers"]
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|t| t == "T3"))
        {
            continue;
        }
        if scenario["id"] == "m4-spoof-tool" {
            writeln!(
                output,
                "m4-spoof-tool: not-runnable (requires W2 assembled bridge/agent integration)"
            )
            .map_err(|_| "sim suite: stdout failed")?;
            unavailable += 1;
            continue;
        }
        let mut command = vec![
            name.into(),
            "--driver".into(),
            "sim".into(),
            "--bridge".into(),
            bridge.to_string_lossy().into_owned(),
        ];
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
