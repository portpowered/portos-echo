use std::io::Write;
use std::path::{Path, PathBuf};

pub fn execute(args: &[String], output: &mut impl Write) -> Result<(), String> {
    let (extras, root, partition) = options(args)?;
    let (bridge, paths) = discover()?;
    run_suite(&bridge, paths, &extras, root.as_deref(), partition, output)
}

/// `--partition K/N` keeps every Nth T3 scenario, starting at the Kth, in sorted order, so the
/// partitions 1/N to N/N together run each T3 scenario exactly once.
fn partition(value: &str) -> Result<(usize, usize), String> {
    let parsed = value
        .split_once('/')
        .and_then(|(k, n)| Some((k.parse::<usize>().ok()?, n.parse::<usize>().ok()?)));
    match parsed {
        Some((k, n)) if (1..=n).contains(&k) => Ok((k, n)),
        _ => Err("sim suite: --partition expects K/N with 1 <= K <= N".into()),
    }
}

const fn selected(runnable: usize, (shard, shards): (usize, usize)) -> bool {
    runnable % shards == shard - 1
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
                Some("m3-full-duplex" | "m4-spoof-tool" | "m6-emotion" | "m7-muse")
            )
            || scenario["id"].as_str().is_some_and(|id| id.starts_with("session-"))
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
        .with_file_name(name);
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
        .with_file_name("echo-bridge");
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
    (shard, shards): (usize, usize),
    output: &mut impl Write,
) -> Result<(), String> {
    let mut passed = 0;
    let mut failed = 0;
    let unavailable = 0;
    let mut runnable = 0;
    for path in paths {
        let name = path.to_str().ok_or("sim suite: invalid scenario path")?;
        let scenario = crate::lab_validate::load(name)?;
        if !scenario["tiers"]
            .as_array()
            .is_some_and(|tiers| tiers.iter().any(|t| t == "T3"))
        {
            continue;
        }
        runnable += 1;
        if !selected(runnable - 1, (shard, shards)) {
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
            Some("m3-full-duplex" | "m4-spoof-tool" | "m6-emotion" | "m7-muse")
        )
        || scenario["id"].as_str().is_some_and(|id| id.starts_with("session-"))
    {
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

type SuiteOptions = (Vec<String>, Option<PathBuf>, (usize, usize));

fn options(args: &[String]) -> Result<SuiteOptions, String> {
    let mut extras = Vec::new();
    let mut root = None;
    let mut shard = (1, 1);
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
            "--partition" => {
                index += 1;
                shard = partition(args.get(index).ok_or("sim suite: missing option value")?)?;
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
    Ok((extras, root, shard))
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
        .with_file_name("echo-bridge");
    if !bridge.is_file() {
        return Err("m4: missing prebuilt echo-bridge; run cargo build -p echo-bridge --bins --locked".into());
    }
    Ok(bridge)
}

#[cfg(test)]
mod tests {
    use super::{options, partition, selected};

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn partitions_cover_each_runnable_index_exactly_once() {
        for shards in 1..=4 {
            let mut seen = vec![0; 17];
            for shard in 1..=shards {
                let parsed = partition(&format!("{shard}/{shards}")).unwrap();
                assert_eq!(parsed, (shard, shards));
                for (index, count) in seen.iter_mut().enumerate() {
                    *count += usize::from(selected(index, parsed));
                }
            }
            assert!(seen.iter().all(|count| *count == 1));
        }
    }

    #[test]
    fn partition_option_is_strict_and_defaults_to_everything() {
        assert_eq!(options(&args("--all --driver sim")).unwrap().2, (1, 1));
        assert_eq!(options(&args("--all --driver sim --partition 2/3")).unwrap().2, (2, 3));
        for bad in ["0/2", "3/2", "1/0", "1", "a/b", "1/2/3", "-1/2", "--out"] {
            assert!(
                options(&args(&format!("--all --driver sim --partition {bad}"))).is_err(),
                "{bad}"
            );
        }
        assert!(options(&args("--all --driver sim --partition")).is_err());
        assert!(options(&args("--all --driver sim --partition 1/2 --partition 2/2")).is_err());
    }
}
