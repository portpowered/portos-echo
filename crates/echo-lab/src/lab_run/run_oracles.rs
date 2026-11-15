//! Shared oracle semantics. Drivers supply recordings and screen bytes.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Step, run_command};

fn pattern(value: &str) -> Result<jsonschema::Validator, String> {
    if value.is_empty() {
        return Err("speech expectation: empty pattern".into());
    }
    jsonschema::validator_for(&json!({"type":"string", "pattern":value}))
        .map_err(|_| "speech expectation: invalid pattern".into())
}

pub fn parse(value: &Value) -> Result<Step, String> {
    if let Some(phrases) = value["expect_speech_contains"].as_array() {
        let phrases = phrases
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if phrases.is_empty() || phrases.iter().any(|p| p.trim().is_empty()) {
            return Err("speech expectation: empty phrases".into());
        }
        for phrase in &phrases {
            if let Some(regex) = phrase.strip_prefix("regex:") {
                pattern(regex)?;
            }
        }
        return Ok(Step::Speech(phrases));
    }
    if let Some(screen) = value["expect_screen"].as_object() {
        if screen.len() != 2 || screen.get("tolerance") != Some(&json!(0)) {
            return Err("screen expectation: requires golden and tolerance: 0".into());
        }
        let golden = PathBuf::from(
            screen
                .get("golden")
                .and_then(Value::as_str)
                .ok_or("screen: missing golden")?,
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/goldens");
        let path = if golden.is_absolute() {
            golden
        } else {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(golden)
        };
        let path = path.canonicalize().map_err(|_| "screen: missing golden PNG")?;
        if !path.starts_with(root.canonicalize().map_err(|_| "screen: missing golden directory")?)
            || path.extension().is_none_or(|ext| ext != "png")
        {
            return Err("screen: golden must be a PNG under tests/goldens".into());
        }
        return Ok(Step::Screen(path));
    }
    if let Some(frame) = value["expect_frame"].as_str() {
        crate::lab_validate::frame_path(frame)?;
        return Ok(Step::Frame(frame.into()));
    }
    Ok(Step::Unsupported)
}

pub fn speech(transcript: &[u8], phrases: &[String]) -> Result<(), String> {
    let text = std::str::from_utf8(transcript).map_err(|_| "speech expectation: STT returned invalid text")?;
    if text.trim().is_empty() {
        return Err("speech expectation: STT returned empty text".into());
    }
    for phrase in phrases {
        let matched = if let Some(regex) = phrase.strip_prefix("regex:") {
            pattern(regex)?.is_valid(&json!(text))
        } else {
            text.to_lowercase().contains(&phrase.to_lowercase())
        };
        if !matched {
            return Err("speech expectation: recorded reply did not match".into());
        }
    }
    Ok(())
}

pub fn screen(python: &str, captured: &Path, golden: &Path) -> Result<(), String> {
    let result = run_command::helper(
        python,
        "pixels",
        &[
            captured.to_string_lossy().into_owned(),
            golden.to_string_lossy().into_owned(),
        ],
    )
    .map_err(|_| "screen expectation: PNG decode failed")?;
    match result["different_pixels"].as_u64() {
        Some(0) => Ok(()),
        Some(_) => Err("screen expectation: decoded pixels differ".into()),
        None => Err("screen expectation: dimensions differ or invalid PNG".into()),
    }
}

pub fn frame_destination(root: &Path, name: &str) -> Result<PathBuf, String> {
    crate::lab_validate::frame_path(name)?;
    let canonical = root
        .canonicalize()
        .map_err(|_| "frame capture: invalid run directory")?;
    let root = canonical.as_path();
    let path = root.join(name);
    let parent = path.parent().ok_or("frame capture: missing parent")?;
    let mut current = root.to_path_buf();
    for part in parent
        .strip_prefix(root)
        .map_err(|_| "frame capture: invalid path")?
        .components()
    {
        current.push(part);
        if current.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("frame capture: symlink destination refused".into());
        }
        if !current.exists() {
            std::fs::create_dir(&current).map_err(|_| "frame capture: cannot create directory")?;
        }
    }
    if !parent
        .canonicalize()
        .map_err(|_| "frame capture: invalid directory")?
        .starts_with(root)
        || path.symlink_metadata().is_ok()
    {
        return Err("frame capture: destination exists or escapes run directory".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_and_opt_in_patterns_have_distinct_semantics() {
        assert_eq!(
            speech(b"TEST Echo [a.*]", &["test".into(), "echo".into(), "[a.*]".into()]),
            Ok(())
        );
        assert_eq!(
            speech(
                b"The time is 7:49 p.m.",
                &[r"regex:(?i)\b[0-9]{1,2}:[0-5][0-9]\b".into()]
            ),
            Ok(())
        );
        assert_eq!(
            speech(b"abc", &["a.*".into()]),
            Err("speech expectation: recorded reply did not match".into())
        );
        assert_eq!(
            speech(b"", &["a".into()]),
            Err("speech expectation: STT returned empty text".into())
        );
        for phrase in ["", "regex:", "regex:["] {
            assert!(parse(&json!({"expect_speech_contains":[phrase]})).is_err());
        }
    }

    #[test]
    fn frame_destinations_reject_existing_files_and_links_outside_the_run() {
        let root = std::env::temp_dir().join(format!("lab-frame-path-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let run = root.join("run");
        let outside = root.join("outside");
        std::fs::create_dir(&run).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let run = run.canonicalize().unwrap();
        let link = run.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        assert!(
            std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&link)
                .arg(&outside)
                .output()
                .unwrap()
                .status
                .success()
        );
        assert_eq!(
            frame_destination(&run, "link/frame.jpg"),
            Err("frame capture: symlink destination refused".into())
        );
        assert_eq!(
            frame_destination(&run, "nested/frame.jpg"),
            Ok(run.join("nested/frame.jpg"))
        );
        std::fs::write(run.join("frame.jpg"), b"existing").unwrap();
        assert_eq!(
            frame_destination(&run, "frame.jpg"),
            Err("frame capture: destination exists or escapes run directory".into())
        );
        assert_eq!(std::fs::read(run.join("frame.jpg")).unwrap(), b"existing");
        assert!(!outside.join("frame.jpg").exists());
        #[cfg(windows)]
        std::fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
