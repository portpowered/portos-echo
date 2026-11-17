const ALLOWED: &[&str] = &[
    "--driver",
    "--bench",
    "--port",
    "--adb",
    "--stt",
    "--ring",
    "--out",
    "--python",
    "--keep-audio",
    "--speech-endpoint",
    "--speech-model",
    "--cancel-file",
    "--bridge",
    "--bridge-config",
    "--operator-confirm",
];

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct Options {
    pub sim: bool,
    pub bridge: Option<PathBuf>,
    pub bridge_config: Option<PathBuf>,
    pub scenarios: Vec<String>,
    pub port: PathBuf,
    pub adb: PathBuf,
    pub stt: PathBuf,
    pub python: String,
    pub ring: String,
    pub out: PathBuf,
    pub keep_audio: bool,
    pub speech: Option<(String, String)>,
    pub cancel: Option<PathBuf>,
    pub operator_confirm: Option<PathBuf>,
}

fn required<'a>(flags: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, String> {
    flags
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("run requires {name}"))
}

fn explicit(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.components().count() < 2 || !path.is_file() {
        return Err("run: --port and --adb require explicit executable paths".into());
    }
    path.canonicalize()
        .map_err(|_| "run: cannot resolve explicit executable".into())
}

fn collect(args: &[String]) -> Result<(Vec<String>, BTreeMap<String, String>), String> {
    let mut paths = Vec::new();
    let mut flags = BTreeMap::new();
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if arg.starts_with("--") {
            let value = if arg == "--keep-audio" {
                "true".to_owned()
            } else {
                index += 1;
                args.get(index)
                    .filter(|s| !s.is_empty() && !s.starts_with("--"))
                    .ok_or("run: missing flag value")?
                    .clone()
            };
            if flags.insert(arg.clone(), value).is_some() {
                return Err("run: duplicate flag".into());
            }
        } else {
            paths.push(arg.clone());
        }
        index += 1;
    }
    Ok((paths, flags))
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let (scenarios, flags) = collect(args)?;
    if flags.keys().any(|key| !ALLOWED.contains(&key.as_str())) || scenarios.is_empty() {
        return Err("run: unknown option or missing scenario".into());
    }
    if required(&flags, "--driver")? == "sim" {
        return sim(scenarios, &flags);
    }
    if required(&flags, "--driver")? != "device" || required(&flags, "--bench")? != "ring" {
        return Err("run: only --driver device --bench ring is implemented".into());
    }
    required(&flags, "--stt")?;
    let speech = match (flags.get("--speech-endpoint"), flags.get("--speech-model")) {
        (None, None) => None,
        (Some(endpoint), Some(model)) => Some((endpoint.clone(), model.clone())),
        _ => return Err("run: text speech needs both --speech-endpoint and --speech-model".into()),
    };
    let ring = flags
        .get("--ring")
        .cloned()
        .or_else(|| std::env::var("ECHO_LAB_RING").ok())
        .filter(|s| !s.is_empty())
        .ok_or("run requires --ring or ECHO_LAB_RING")?;
    let out = flags.get("--out").map_or_else(
        || {
            std::env::temp_dir().join(format!(
                "echo-lab-run-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ))
        },
        PathBuf::from,
    );
    Ok(Options {
        sim: false,
        operator_confirm: flags.get("--operator-confirm").map(PathBuf::from),
        bridge: device_bridge(&flags)?,
        bridge_config: flags
            .get("--bridge-config")
            .map(|v| configuration_path(v))
            .transpose()?,
        scenarios,
        port: explicit(required(&flags, "--port")?)?,
        adb: explicit(required(&flags, "--adb")?)?,
        stt: PathBuf::from(required(&flags, "--stt")?),
        python: flags.get("--python").cloned().unwrap_or_else(|| "python".into()),
        ring,
        out,
        keep_audio: flags.contains_key("--keep-audio"),
        speech,
        cancel: flags.get("--cancel-file").map(PathBuf::from),
    })
}

fn sim(scenarios: Vec<String>, flags: &BTreeMap<String, String>) -> Result<Options, String> {
    if scenarios.len() != 1
        || flags
            .keys()
            .any(|k| !["--driver", "--bridge", "--out", "--python", "--cancel-file"].contains(&k.as_str()))
    {
        return Err("sim: requires one scenario and sim options only".into());
    }
    let scenario = crate::lab_validate::load(&scenarios[0])?;
    let bridge = selected_bridge(flags, &scenario)?;
    let milestone = scenario["milestone"].as_str().ok_or("sim: missing milestone")?;
    if !milestone
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err("sim: milestone must be a portable directory name".into());
    }
    let base = std::env::temp_dir()
        .join("echo-lab-artifacts")
        .join("artifacts")
        .join(milestone);
    if !flags.contains_key("--out") {
        std::fs::create_dir_all(&base).map_err(|_| "sim: cannot create artifact parent")?;
    }
    let out = flags.get("--out").map_or_else(
        || {
            base.join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ))
        },
        PathBuf::from,
    );
    Ok(Options {
        sim: true,
        operator_confirm: None,
        bridge: Some(bridge),
        bridge_config: None,
        scenarios,
        port: PathBuf::new(),
        adb: PathBuf::new(),
        stt: PathBuf::new(),
        python: flags.get("--python").cloned().unwrap_or_else(|| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        }),
        ring: String::new(),
        out,
        keep_audio: false,
        speech: None,
        cancel: flags.get("--cancel-file").map(PathBuf::from),
    })
}

fn device_bridge(flags: &BTreeMap<String, String>) -> Result<Option<PathBuf>, String> {
    match (flags.get("--bridge"), flags.get("--bridge-config")) {
        (None, None) => Ok(None),
        (Some(bridge), Some(_)) => explicit(bridge)
            .map(Some)
            .map_err(|_| "device: --bridge requires an explicit existing executable".into()),
        _ => Err("device: supply --bridge and --bridge-config together".into()),
    }
}

fn configuration_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if !path.is_file() {
        return Err("device: --bridge-config requires a readable TOML file".into());
    }
    path.canonicalize()
        .map_err(|_| "device: cannot resolve --bridge-config".into())
}

pub fn destination(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .filter(|name| *name != "." && *name != "..")
        .ok_or("run output: expected a fresh directory name")?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let target = parent
        .canonicalize()
        .map_err(|_| "run output: parent must exist")?
        .join(name);
    if target.exists() {
        return Err("run output: destination already exists".into());
    }
    let output = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain", "-z"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .map_err(|_| "run output: cannot inspect worktrees")?;
    if !output.status.success() {
        return Err("run output: cannot inspect worktrees".into());
    }
    check_worktrees(&output.stdout, &target, |root| {
        resolved_worktree(root.canonicalize(), || std::fs::symlink_metadata(root).map(|_| ()))
    })?;
    if target.ancestors().any(|ancestor| ancestor.join(".git").exists()) {
        return Err("run output: artifacts must be outside git".into());
    }
    Ok(target)
}

fn check_worktrees(
    listing: &[u8],
    target: &Path,
    mut resolve: impl FnMut(&Path) -> Result<Option<PathBuf>, String>,
) -> Result<(), String> {
    let fields: Vec<_> = listing.split(|byte| *byte == 0).collect();
    for record in fields.split(|field| field.is_empty()) {
        if record
            .iter()
            .any(|field| *field == b"prunable" || field.starts_with(b"prunable "))
        {
            continue;
        }
        for field in record {
            if let Some(root) = std::str::from_utf8(field)
                .ok()
                .and_then(|s| s.strip_prefix("worktree "))
                && let Some(root) = resolve(Path::new(root))?
                && target.starts_with(root)
            {
                return Err("run output: artifacts must be outside all repository worktrees".into());
            }
        }
    }
    Ok(())
}

fn resolved_worktree(
    canonical: std::io::Result<PathBuf>,
    metadata: impl FnOnce() -> std::io::Result<()>,
) -> Result<Option<PathBuf>, String> {
    match canonical {
        Ok(root) => Ok(Some(root)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            _ => Err("run output: cannot resolve worktree".into()),
        },
        Err(_) => Err("run output: cannot resolve worktree".into()),
    }
}

fn selected_bridge(flags: &BTreeMap<String, String>, scenario: &serde_json::Value) -> Result<PathBuf, String> {
    Ok(if let Some(value) = flags.get("--bridge") {
        explicit(value)
            .map_err(|_| format!("sim: --bridge requires an explicit executable path to an existing file: {value}"))?
    } else {
        super::sim::suite::scenario_bridge(scenario)?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_bridge_pair_is_required_before_tool_paths() {
        let base = [
            "scenario.yaml",
            "--driver",
            "device",
            "--bench",
            "ring",
            "--stt",
            "unused",
            "--ring",
            "fixture",
        ];
        for option in ["--bridge", "--bridge-config"] {
            let mut args: Vec<String> = base.iter().map(|s| (*s).into()).collect();
            args.extend([option.into(), "./missing".into()]);
            assert_eq!(
                parse(&args).err().unwrap(),
                "device: supply --bridge and --bridge-config together"
            );
        }
    }
}

#[cfg(test)]
mod worktree_tests {
    use super::{check_worktrees, resolved_worktree};
    use std::io::{Error, ErrorKind};
    use std::path::{Path, PathBuf};

    #[test]
    fn worktree_records_skip_prunable_and_keep_live_siblings_and_final_record() {
        for marker in ["prunable", "prunable gitdir file points to non-existent location"] {
            let listing = format!(
                "worktree /missing\0detached\0{marker}\0\0worktree /live space\nline\0branch refs/heads/main\0locked reason\0"
            );
            let mut roots = Vec::new();
            let result = check_worktrees(listing.as_bytes(), Path::new("/live space\nline/output"), |root| {
                roots.push(root.to_owned());
                Ok(Some(root.to_owned()))
            });
            assert_eq!(roots, [PathBuf::from("/live space\nline")]);
            assert_eq!(
                result.unwrap_err(),
                "run output: artifacts must be outside all repository worktrees"
            );
        }
    }

    #[test]
    fn worktree_external_output_and_absent_unmarked_records_are_accepted() {
        let mut roots = Vec::new();
        assert_eq!(
            check_worktrees(
                b"worktree /absent\0\0worktree /live\0\0",
                Path::new("/external/out"),
                |root| {
                    roots.push(root.to_owned());
                    Ok((root == Path::new("/live")).then(|| root.to_owned()))
                }
            ),
            Ok(())
        );
        assert_eq!(roots, [PathBuf::from("/absent"), PathBuf::from("/live")]);
    }

    #[test]
    fn worktree_resolution_skips_only_confirmed_absence() {
        assert_eq!(
            resolved_worktree(Ok(PathBuf::from("/live")), || unreachable!()),
            Ok(Some(PathBuf::from("/live")))
        );
        assert_eq!(
            resolved_worktree(Err(Error::from(ErrorKind::NotFound)), || Err(Error::from(
                ErrorKind::NotFound
            ))),
            Ok(None)
        );
        for metadata in [Ok(()), Err(ErrorKind::PermissionDenied), Err(ErrorKind::Other)] {
            assert_eq!(
                resolved_worktree(Err(Error::from(ErrorKind::NotFound)), || metadata.map_err(Error::from)).unwrap_err(),
                "run output: cannot resolve worktree"
            );
        }
        let result = check_worktrees(b"worktree /live\0\0", Path::new("/external/out"), |_| {
            resolved_worktree(Err(Error::from(ErrorKind::PermissionDenied)), || unreachable!())
        });
        assert_eq!(result.unwrap_err(), "run output: cannot resolve worktree");
    }
}
