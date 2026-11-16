use serde_json::{Map, Value};

#[derive(Debug)]
pub struct Selector {
    event: String,
    fields: Map<String, Value>,
    occurrences: usize,
    frames: Option<u64>,
}

impl Selector {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (name, mapping) = text
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((text.trim(), "{}"));
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b)) {
            return Err("trace selector: invalid event name".into());
        }
        if !mapping.trim().starts_with('{') || !mapping.trim().ends_with('}') || mapping.contains('\n') {
            return Err("trace selector: expected flow mapping".into());
        }
        let value: Value = serde_norway::from_str(mapping.trim()).map_err(|_| "trace selector: invalid mapping")?;
        let mut fields = value
            .as_object()
            .cloned()
            .ok_or("trace selector: expected flow mapping")?;
        let occurrences = match fields.remove("count") {
            None => 1,
            Some(count) => count
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or("trace selector: count must be a positive integer")?,
        };
        let frames = fields
            .remove("count_gte")
            .map(|v| v.as_u64().ok_or("trace selector: invalid count_gte"))
            .transpose()?;
        if name == "mic.mute_window"
            && (fields
                .get("window_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
                || fields.get("received_frames") != Some(&Value::from(0))
                || fields.get("forwarded_frames") != Some(&Value::from(0))
                || occurrences != 1
                || frames.is_some())
        {
            return Err("mute window selector requires window_id, received_frames: 0 and forwarded_frames: 0".into());
        }
        if fields.iter().any(|(key, value)| {
            key == "event"
                || key.ends_with("_gte")
                || key.ends_with("_lte")
                || key.ends_with("_gt")
                || key.ends_with("_lt")
                || value.is_array()
                || value.is_object()
                || value.is_null()
        }) {
            return Err("trace selector: unsupported operator or non-scalar field".into());
        }
        Ok(Self {
            event: name.to_owned(),
            fields,
            occurrences,
            frames,
        })
    }

    fn matches(&self, record: &Value) -> bool {
        record["event"] == self.event
            && self.fields.iter().all(|(k, v)| record.get(k) == Some(v))
            && self
                .frames
                .is_none_or(|min| record["count"].as_u64().is_some_and(|n| n >= min))
    }
}

pub fn parse(values: &[Value]) -> Result<Vec<Selector>, String> {
    if values.is_empty() {
        return Err("trace expectation: empty selectors".into());
    }
    values
        .iter()
        .map(|v| Selector::parse(v.as_str().ok_or("trace selector: expected string")?))
        .collect()
}

/// One run must satisfy the entire ordered sequence. Finalization rechecks all
/// occurrences so a late duplicate cannot be hidden behind an early match.
pub fn check(records: &[Value], selectors: &[Selector], run: Option<&str>) -> Result<(), String> {
    if selectors.is_empty() {
        return Err("trace expectation: empty selectors".into());
    }
    let mut cursor = 0;
    let mut correlated = run.map(str::to_owned);
    for (index, selector) in selectors.iter().enumerate() {
        let mute_window = selector.event == "mic.mute_window";
        if mute_window {
            muted_frame_oracle(records, &selector.fields["window_id"])?;
        }

        let candidates = records
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                selector.matches(r)
                    && (mute_window || correlated.as_deref().is_none_or(|id| r["run_id"].as_str() == Some(id)))
            })
            .collect::<Vec<_>>();
        if candidates.len() != selector.occurrences {
            return Err(format!(
                "trace expectation {index} ({}): expected {} occurrences, observed {}",
                selector.event,
                selector.occurrences,
                candidates.len()
            ));
        }
        if candidates.iter().any(|(position, _)| *position < cursor) {
            return Err(format!(
                "trace expectation {index} ({}): wrong event order",
                selector.event
            ));
        }
        if let Some((position, last)) = candidates.last() {
            cursor = position + 1;
            if correlated.is_none() {
                correlated = last["run_id"].as_str().map(str::to_owned);
            }
        }
    }
    Ok(())
}

// A no-run privacy window must carry explicit, fresh measurements. Missing
// values are never interpreted as zero, and duplicate IDs are ambiguous.
fn muted_frame_oracle(records: &[Value], window: &Value) -> Result<(), String> {
    let matching: Vec<_> = records
        .iter()
        .filter(|r| r["event"] == "mic.mute_window" && &r["window_id"] == window)
        .collect();
    if matching.len() != 1 {
        return Err(format!(
            "mute window {window}: expected 1 measurement, observed {}",
            matching.len()
        ));
    }
    for field in ["received_frames", "forwarded_frames"] {
        let count = matching[0][field]
            .as_u64()
            .ok_or_else(|| format!("mute window {window}: missing or invalid {field}"))?;
        if count != 0 {
            return Err(format!("mute window {window} {field}: expected 0, observed {count}"));
        }
    }
    Ok(())
}
