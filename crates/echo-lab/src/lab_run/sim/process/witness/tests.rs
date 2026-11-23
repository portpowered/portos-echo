use super::*;
use serde_json::{Value, json};

fn clean() -> Value {
    json!({
        "status": "pass",
        "snapshot": {
            "device": {
                "connected": false, "registered": false, "run_open": false, "capture_open": false,
                "mic_muted": false, "mute_latched": false, "speaker_volume": null, "speaker_muted": null
            },
            "trace": {"pending_rows": 0, "writer_failed": false}
        },
        "diff": null
    })
}

fn generation(root: &Path, name: &str, value: Option<&Value>) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir(&dir).unwrap();
    if let Some(value) = value {
        fs::write(dir.join(REPORT_FILE), serde_json::to_vec(value).unwrap()).unwrap();
    }
    dir
}

#[test]
fn t1_world_witness_requires_a_valid_result_from_each_generation() {
    let root = tempfile::tempdir().unwrap();
    let first = generation(root.path(), "first", Some(&clean()));
    let second = generation(root.path(), "second", None);
    assert_eq!(collect(std::slice::from_ref(&first)).ok().unwrap().len(), 1);
    assert_eq!(collect(&[first.clone(), second.clone()]).err(), Some(MISSING));
    let mut bad = clean();
    bad["snapshot"]["device"]
        .as_object_mut()
        .unwrap()
        .remove("speaker_muted");
    fs::write(second.join(REPORT_FILE), bad.to_string()).unwrap();
    assert_eq!(collect(&[first.clone(), second.clone()]).err(), Some(INVALID));
    fs::write(second.join(REPORT_FILE), clean().to_string()).unwrap();
    fs::write(second.join("second.json"), clean().to_string()).unwrap();
    assert_eq!(collect(&[first, second]).err(), Some(INVALID));
}

#[test]
fn t1_world_witness_prints_one_safe_first_diff_and_preserves_oracle_error() {
    let root = tempfile::tempdir().unwrap();
    let mut failure = clean();
    let first_diff = "device.capture_open: expected false, actual true";
    failure["status"] = json!("fail");
    failure["diff"] = json!(first_diff);
    let first = generation(root.path(), "first", Some(&failure));
    let later = generation(root.path(), "later", None);
    let mut output = Vec::new();
    let world = collect(&[first, later]).map(|_| ());
    assert_eq!(
        emit(&mut output, "fixture", world, Err("wire oracle".into()), false),
        Err("wire oracle".into())
    );
    assert_eq!(
        String::from_utf8(output).unwrap(),
        format!("world invariant scenario=fixture status=fail\nWorld diff: {first_diff}\n")
    );
    let mut output = Vec::new();
    assert_eq!(
        emit(&mut output, "fixture", Ok(()), Err("wire oracle".into()), false),
        Err("wire oracle".into())
    );
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "world invariant scenario=fixture status=pass\nWorld diff: none (invariants hold)\n"
    );
}

#[test]
fn t1_world_witness_rejects_an_unredacted_diff_and_an_inconsistent_pass() {
    let root = tempfile::tempdir().unwrap();
    let mut passed = clean();
    passed["snapshot"]["device"]["capture_open"] = json!(true);
    let mut failed = clean();
    failed["status"] = json!("fail");
    failed["diff"] = json!("arbitrary child text");
    for (name, report) in [("passed", passed), ("failed", failed)] {
        let dir = generation(root.path(), name, Some(&report));
        assert_eq!(collect(&[dir]).err(), Some(INVALID));
    }
}
