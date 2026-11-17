mod probe_adb;
pub(crate) mod probe_command;
pub(crate) mod probe_parse;
mod probe_profiles;
pub(crate) mod probe_stream;
#[cfg(test)]
mod probe_tests;
mod probe_time;

use serde_json::Value;
use std::io::Write;
use std::path::Path;

fn arguments(args: &[String]) -> Result<(&Path, &Path), String> {
    let [first, one, second, two] = args else {
        return Err("probe requires --adb <path-to-adb> and --out <fresh-dir>".into());
    };
    match (first.as_str(), second.as_str()) {
        ("--adb", "--out") if !one.is_empty() && !two.is_empty() => Ok((Path::new(one), Path::new(two))),
        ("--out", "--adb") if !one.is_empty() && !two.is_empty() => Ok((Path::new(two), Path::new(one))),
        _ => Err("probe requires exactly one --adb and one --out".into()),
    }
}

fn publish(path: &Path, raw: &[probe_adb::Observation], text: &str) -> Result<(), String> {
    std::fs::create_dir(path).map_err(|_| "probe output: cannot create fresh directory")?;
    let raw_path = path.join("raw");
    std::fs::create_dir(&raw_path).map_err(|_| "probe output: cannot create raw directory")?;
    std::fs::write(raw_path.join(".gitignore"), b"*\n").map_err(|_| "probe output: cannot protect raw files")?;
    for (index, observation) in raw.iter().enumerate() {
        std::fs::write(raw_path.join(format!("{index:02}.stdout.txt")), &observation.stdout)
            .map_err(|_| "probe output: cannot write raw stdout")?;
        std::fs::write(raw_path.join(format!("{index:02}.stderr.txt")), &observation.stderr)
            .map_err(|_| "probe output: cannot write raw stderr")?;
    }
    let pending = path.join("manifest.pending");
    std::fs::write(&pending, text).map_err(|_| "probe output: cannot write manifest")?;
    std::fs::rename(pending, path.join("manifest.json")).map_err(|_| "probe output: cannot publish manifest".into())
}

fn manifest(raw: &[probe_adb::Observation]) -> Result<Value, String> {
    probe_parse::manifest(raw, &probe_time::now()?)
}

fn emit(text: &str, output: &mut impl Write) -> Result<(), String> {
    output
        .write_all(text.as_bytes())
        .map_err(|_| "probe stdout: write failed".into())
}

pub fn run(args: &[String], output: &mut impl Write, errors: &mut impl Write) -> u8 {
    let result: Result<u8, String> = (|| {
        let (adb, path) = arguments(args)?;
        if path
            .try_exists()
            .map_err(|_| "probe output: cannot inspect destination")?
        {
            return Err("probe output: destination already exists".into());
        }
        let (raw, serials) = match probe_adb::collect(adb)? {
            probe_adb::Collection::Refused(device) => {
                let _ = writeln!(errors, "probe refuses device {device:?}; expected athena");
                return Ok(2);
            }
            probe_adb::Collection::Athena { raw, serials } => (raw, serials),
        };
        let text = probe_adb::scrub(
            &format!(
                "{}\n",
                serde_json::to_string_pretty(&manifest(&raw).map_err(|e| probe_adb::scrub(&e, &serials))?)
                    .map_err(|_| "probe JSON encoding failed")?
            ),
            &serials,
        );
        validate_manifest(&text)?;
        publish(path, &raw, &text)?;
        emit(&text, output)?;
        Ok(0)
    })();
    match result {
        Ok(code) => code,
        Err(message) => {
            let _ = writeln!(errors, "echo-lab: {message}");
            1
        }
    }
}

fn validate_manifest(text: &str) -> Result<(), String> {
    let schema = serde_json::from_str(include_str!("../../data/probe.schema.json"))
        .map_err(|_| "probe schema: invalid embedded JSON")?;
    let validator = jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .map_err(|_| "probe schema: invalid embedded schema")?;
    let value = serde_json::from_str(text).map_err(|_| "probe manifest: invalid sanitized JSON")?;
    if !validator.is_valid(&value) {
        return Err("probe manifest: invalid sanitized observations".into());
    }
    Ok(())
}
