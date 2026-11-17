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
