use super::probe_adb::Observation;
use serde_json::{Value, json};
#[path = "probe_inputs.rs"]
pub(crate) mod probe_inputs;

const PIN: &str = include_str!("../../data/athena-7080.md5");
const MIXER: &str = "Master TAS5805m Playback Volume";

fn required(text: &str, label: &str) -> Result<String, String> {
    let value = text.trim();
    if value.is_empty() || value.lines().count() != 1 {
        return Err(format!("probe {label}: missing or malformed value"));
    }
    Ok(value.to_owned())
}

pub fn positive(text: &str, label: &str) -> Result<u32, String> {
    text.trim()
        .parse::<u32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("probe {label}: expected positive integer"))
}

pub fn hash(text: &str, path: &str) -> Result<String, String> {
    let fields: Vec<_> = text.split_whitespace().collect();
    match fields.as_slice() {
        [digest, observed_path]
            if *observed_path == path && digest.len() == 32 && digest.bytes().all(|c| c.is_ascii_hexdigit()) =>
        {
            Ok(digest.to_ascii_lowercase())
        }
        _ => Err(format!("probe md5sum {path}: malformed hash or path")),
    }
}

fn hashes(raw: &[Observation]) -> Result<Vec<Value>, String> {
    PIN.lines()
        .zip(&raw[7..13])
        .map(|(line, observed)| {
            let (pinned, relative) = line.split_once("  ").ok_or("probe pinned hash: malformed reference")?;
            let path = format!("/system/{relative}");
            let md5 = hash(&observed.stdout, &path)?;
            Ok(json!({"path": path, "md5": md5, "pinned_md5": pinned,
            "comparison": if md5 == pinned { "match" } else { "differs" }}))
        })
        .collect()
}

pub fn display_value(text: &str, label: &str) -> Result<String, String> {
    let mut physical = None;
    let mut override_value = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if let Some(value) = line.trim().strip_prefix(&format!("Physical {label}:")) {
            if physical.replace(value.trim().to_owned()).is_some() {
                return Err(format!("probe display {label}: duplicate physical value"));
            }
        } else if let Some(value) = line.trim().strip_prefix(&format!("Override {label}:")) {
            if override_value.replace(value.trim().to_owned()).is_some() {
                return Err(format!("probe display {label}: duplicate override value"));
            }
        } else {
            return Err(format!("probe display {label}: malformed observation"));
        }
    }
    override_value
        .or(physical)
        .ok_or_else(|| format!("probe display {label}: missing value"))
}

pub fn display(size: &str, density: &str) -> Result<Value, String> {
    let size = display_value(size, "size")?;
    let (width, height) = size.split_once('x').ok_or("probe display size: malformed dimensions")?;
    Ok(
        json!({"width": positive(width, "display width")?, "height": positive(height, "display height")?,
        "density": positive(&display_value(density, "density")?, "display density")?}),
    )
}

pub fn mixer(text: &str) -> Result<Value, String> {
    let mut values = None;
    for line in text.lines() {
        let tokens: Vec<_> = line.split_whitespace().collect();
        if tokens.len() < 4 || tokens[0].parse::<u32>().is_err() {
            continue;
        }
        let tail = tokens[3..].join(" ");
        let Some(tail) = tail.strip_prefix(MIXER).and_then(|s| s.strip_prefix(' ')) else {
            continue;
        };
        if values.is_some() {
            return Err("probe tinymix: duplicate named control".into());
        }
        let parsed: Vec<i64> = tail
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|_| "probe tinymix: malformed control values")?;
        if parsed.is_empty() {
            return Err("probe tinymix: missing control values".into());
        }
        values = Some(parsed);
    }
    Ok(json!({"name": MIXER, "values": values.ok_or("probe tinymix: missing named control")?}))
}

pub fn manifest(raw: &[Observation], timestamp: &str) -> Result<Value, String> {
    if raw.len() != 20 {
        return Err("probe: incomplete observation set".into());
    }
    let mut models: Vec<_> = raw[19]
        .stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    models.sort_unstable();
    models.dedup();
    Ok(json!({
        "probe_timestamp": timestamp,
        "source": concat!("echo-lab probe ", env!("CARGO_PKG_VERSION")),
        "build": {"device": required(&raw[2].stdout, "device")?,
            "incremental": required(&raw[3].stdout, "incremental")?,
            "sdk": positive(&raw[4].stdout, "sdk")?,
            "fingerprint": required(&raw[5].stdout, "fingerprint")?,
            "kernel": required(&raw[6].stdout, "kernel")?},
        "hashes": hashes(raw)?,
        "input_devices": probe_inputs::parse(&raw[13].stdout, &raw[14].stdout)?,
        "display": display(&raw[17].stdout, &raw[18].stdout)?,
        "mixer": mixer(&raw[15].stdout)?,
        "keyword_models": models,
    }))
}
