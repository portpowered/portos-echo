//! Semantic comparison of milestone receipts, excluding independent diagnostics.
//! No recording/update path: callers must explicitly retain an observed trace.
use std::collections::BTreeMap;

use serde_json::Value;

use super::super::trace::Trace;

#[derive(Default)]
struct Names {
    ids: BTreeMap<String, BTreeMap<String, usize>>,
    ports: BTreeMap<u16, usize>,
}

impl Names {
    fn field(&mut self, key: &str, value: &mut Value) -> Result<(), String> {
        match key {
            "run_id" | "call_id" | "delegation_id" | "thread_id" | "turn_id" if !value.is_null() => {
                let text = value
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("golden: invalid identity")?;
                let names = self.ids.entry(key.into()).or_default();
                let next = names.len() + 1;
                let index = *names.entry(text.into()).or_insert(next);
                *value = Value::String(format!("{key}-{index}"));
            }
            "address" | "health_addr" => {
                let address: std::net::SocketAddr = value
                    .as_str()
                    .ok_or("golden: invalid endpoint")?
                    .parse()
                    .map_err(|_| "golden: invalid endpoint")?;
                if address.ip() != std::net::Ipv4Addr::LOCALHOST || address.port() == 0 {
                    return Err("golden: endpoint must be bound on 127.0.0.1".into());
                }
                let next = self.ports.len() + 1;
                let index = self.ports.entry(address.port()).or_insert(next);
                *value = Value::String(format!("127.0.0.1:port-{index}"));
            }
            _ => self.nested(value)?,
        }
        Ok(())
    }

    fn nested(&mut self, value: &mut Value) -> Result<(), String> {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    self.field(key, value)?;
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.nested(item)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

pub fn normalize(trace: &Trace) -> Result<Vec<String>, String> {
    trace.finish()?;
    if trace.records.is_empty() {
        return Err("golden: missing actual trace".into());
    }
    let mut names = Names::default();
    milestone_records(trace)
        .iter()
        // Reply quality and the bridge's `timing` samples are measurements with
        // their own budgets, not steps: their values follow the run's pace.
        .filter(|record| {
            !incidental_probe(record) && !matches!(record["event"].as_str(), Some("reply.quality" | "timing"))
        })
        .map(|record| {
            let mut record = record.clone();
            names.nested(&mut record)?;
            record["elapsed_ms"] = Value::from(0);
            serde_json::to_string(&record).map_err(|_| "golden: cannot encode record".into())
        })
        .collect()
}

pub(super) fn milestone_records(trace: &Trace) -> Vec<Value> {
    trace
        .records
        .iter()
        .filter(|record| !delegation_diagnostic(record))
        .cloned()
        .collect()
}

fn delegation_diagnostic(record: &Value) -> bool {
    // The milestone's delegation receipts come from its independent sideband
    // observer. Bridge diagnostics remain in the raw trace and have dedicated
    // configured-owner assertions; they cannot stand in for that native proof.
    record.get("source").is_none()
        && matches!(
            record["event"].as_str(),
            Some("delegation.created" | "delegation.tool" | "delegation.resolved")
        )
}

fn incidental_probe(record: &Value) -> bool {
    record["event"] == "wyoming.probe_ok"
        && record["run_id"] == "connection-1"
        && record["elapsed_ms"].as_u64().is_some()
        && matches!(record["provider_auth_mode"].as_str(), Some("api_key" | "chatgpt"))
        && record.as_object().is_some_and(|fields| fields.len() == 4)
}

pub fn compare(trace: &Trace, expected: &[u8]) -> Result<(), String> {
    // Parse the fixture, but never normalize it: a changed golden timestamp,
    // count, identity relationship, endpoint relationship or event must fail.
    let mut golden = Trace::default();
    golden.append(expected)?;
    golden.finish()?;
    if golden.records.is_empty() {
        return Err("golden: missing expected trace".into());
    }
    let expected = golden.records.iter().map(Value::to_string).collect::<Vec<_>>();
    let actual = normalize(trace)?;
    let mut differences = Vec::new();
    for index in 0..expected.len().max(actual.len()) {
        if expected.get(index) != actual.get(index) {
            differences.push(format!(
                "line {}\n- {}\n+ {}",
                index + 1,
                expected.get(index).map_or("<missing>", String::as_str),
                actual.get(index).map_or("<missing>", String::as_str),
            ));
        }
    }
    if differences.is_empty() {
        Ok(())
    } else {
        Err(format!("golden: trace differs\n{}", differences.join("\n")))
    }
}

#[cfg(test)]
mod tests;
