use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use jsonschema::{Validator, error::ValidationErrorKind};
use serde_json::Value;

const SCHEMA: &str = include_str!("../../../schemas/scenario.schema.json");

fn schema() -> io::Result<Validator> {
    let value: Value = serde_json::from_str(SCHEMA)?;
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&value)
        .map_err(|error| io::Error::other(error.to_string()))
}

fn discover() -> io::Result<Vec<String>> {
    let mut paths = fs::read_dir("tests/scenarios")?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<PathBuf>>>()?;
    paths.retain(|path| path.extension().is_some_and(|extension| extension == "yaml"));
    paths.sort();
    if paths.is_empty() {
        return Err(io::Error::other("no scenario YAML files found"));
    }
    Ok(paths
        .iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect())
}

fn read(path: &str) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_norway::from_str(&text).map_err(|error| error.to_string())
}

pub fn validate(paths: &[String], output: &mut impl Write) -> io::Result<bool> {
    let validator = schema()?;
    let selected = if paths.is_empty() {
        match discover() {
            Ok(paths) => paths,
            Err(error) => {
                writeln!(output, "tests/scenarios/: : {error}")?;
                return Ok(false);
            }
        }
    } else {
        paths.to_vec()
    };
    let mut passed = true;
    for path in selected {
        match read(&path) {
            Ok(value) => passed &= validate_value(&validator, &value, &path, output)?,
            Err(error) => {
                passed = false;
                writeln!(output, "{path}: : {error}")?;
            }
        }
    }
    Ok(passed)
}

fn validate_value(validator: &Validator, value: &Value, path: &str, output: &mut impl Write) -> io::Result<bool> {
    let mut passed = true;
    for error in validator.iter_errors(value) {
        passed = false;
        let mut pointer = error.instance_path.to_string();
        if let ValidationErrorKind::Required { property } = &error.kind
            && let Some(property) = property.as_str()
        {
            pointer.push('/');
            pointer.push_str(&property.replace('~', "~0").replace('/', "~1"));
        }
        writeln!(output, "{path}: {pointer}: {error}")?;
    }
    if passed {
        writeln!(output, "{path}: ok")?;
    }
    Ok(passed)
}

#[cfg(test)]
#[path = "lab_schema_tests.rs"]
mod tests;
