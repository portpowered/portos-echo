use super::super::run_report::Artifacts;
use std::io::Write;

pub fn append(artifacts: &mut Artifacts, value: &serde_json::Value) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(artifacts.root.join("device-steps.jsonl"))
        .map_err(|_| "device: cannot open observations")?;
    writeln!(file, "{value}").map_err(|_| "device: cannot write observations")?;
    if !artifacts.evidence.iter().any(|s| s == "device-steps.jsonl") {
        artifacts.evidence.push("device-steps.jsonl".into());
    }
    Ok(())
}

pub fn mute_window(artifacts: &mut Artifacts, record: &serde_json::Value) -> Result<(), String> {
    if record["event"] != "mic.mute_window" {
        return Ok(());
    }
    append(
        artifacts,
        &serde_json::json!({"event":"device.mute_window","step":null,
        "elapsed_ms":record["elapsed_ms"],"window_id":record["window_id"],
        "received_frames":record["received_frames"],"forwarded_frames":record["forwarded_frames"]}),
    )
}

#[cfg(test)]
mod tests {
    use super::super::runtime::Runtime;
    use serde_json::json;

    #[test]
    fn device_mute_measurement_must_be_fresh_not_an_earlier_window() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trace");
        std::fs::write(
            &path,
            format!(
                "{}\n",
                json!({"event":"mic.mute_window","window_id":"mute-1",
            "received_frames":0,"forwarded_frames":0,"elapsed_ms":1})
            ),
        )
        .unwrap();
        let mut runtime = Runtime::default();
        runtime.trace_path = path;
        runtime.arm_mute().unwrap();
        let selector = [json!(
            "mic.mute_window {window_id: mute-1, received_frames: 0, forwarded_frames: 0}"
        )];
        assert!(runtime.expect_trace(&selector, 0).unwrap_err().contains("observed 0"));
    }
}
