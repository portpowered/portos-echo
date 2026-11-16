use echo_screen_contract::UiState;
use serde_json::Value;

pub fn bytes(path: &std::path::Path, limit: u64, category: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| format!("{category}: missing observation file"))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("{category}: cannot read observation file"))?;
    if u64::try_from(bytes.len()).map_err(|_| "observation: size overflow")? > limit {
        return Err(format!("{category}: observation exceeds size limit"));
    }
    Ok(bytes)
}

pub fn screen(bytes: &[u8], expected: &Value) -> Result<(), String> {
    let typed: UiState =
        serde_json::from_slice(bytes).map_err(|_| "screen expectation: missing or malformed latest UiState")?;
    let actual = serde_json::to_value(typed).map_err(|_| "screen expectation: cannot encode UiState")?;
    let fields = expected
        .as_object()
        .filter(|m| !m.is_empty() && m.contains_key("state"))
        .ok_or("screen expectation: requires state and known fields")?;
    if fields.iter().any(|(key, value)| actual.get(key) != Some(value)) {
        return Err("screen expectation: latest UiState did not match".into());
    }
    Ok(())
}

pub fn transcript<'a>(records: &'a [Value], run: Option<&str>) -> Result<&'a str, String> {
    records
        .iter()
        .rev()
        .find(|r| r["event"] == "live.transcript" && run.is_some_and(|id| r["run_id"] == id))
        .and_then(|r| r["text"].as_str())
        .ok_or_else(|| "speech expectation: missing transcript for current run".into())
}

pub fn budget(records: &[Value], name: &str, ceiling: f64, run: Option<&str>) -> Result<(), String> {
    if name != "first_audio_after_endpoint_ms_p95" {
        return Err("budget: unsupported metric".into());
    }
    if !ceiling.is_finite() || ceiling < 0.0 {
        return Err("budget: invalid ceiling".into());
    }
    let mut samples = records
        .iter()
        .filter(|r| {
            r["event"] == "timing"
                && r["name"] == "first_audio_after_endpoint_ms"
                && run.is_some_and(|id| r["run_id"] == id)
        })
        .map(|r| {
            r["duration_ms"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .ok_or("budget: invalid timing sample")
        })
        .collect::<Result<Vec<_>, _>>()?;
    if samples.is_empty() {
        return Err("budget: no timing samples for current run".into());
    }
    samples.sort_by(f64::total_cmp);
    let rank = (samples.len() * 95).div_ceil(100);
    if samples[rank - 1] > ceiling {
        return Err("budget: p95 exceeded ceiling".into());
    }
    Ok(())
}
