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
        let candidates = records
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                selector.matches(r) && correlated.as_deref().is_none_or(|id| r["run_id"].as_str() == Some(id))
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
