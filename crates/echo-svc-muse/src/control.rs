mod invokes;

use serde_json::{Value, json};

use crate::wire::InvalidWire;

#[cfg(test)]
pub(super) use invokes::HeldInvoke;
pub(super) use invokes::{Handler, Invocations, Reply};

const MAX_CONTROL: usize = 256 * 1024;
const MAX_OUTPUT: usize = 96 * 1024;
const PREFIX: usize = size_of::<u32>();
const REGISTER_ID: &str = "reg-1";
const REPORT: &str = "echo.report.result";
const REPORT_TIMEOUT_MS: u64 = 15_000;

pub(super) fn registration(node_id: &str) -> Value {
    json!({
        "type": "req",
        "id": REGISTER_ID,
        "method": "link.register",
        "params": {
            "node_id": node_id,
            "display_name": "Port OS Echo",
            "platform": "linux",
            "version": env!("CARGO_PKG_VERSION"),
            "device_family": "gadget",
            "model_id": "portos-echo",
            "metadata": {"model_name": "Port OS Echo"},
            "is_wakeup_supported": false,
            "commands_v2": {
                (REPORT): {
                    "description": "Report the result of a live Echo task. This is not user consent.",
                    "required": {
                        "task_id": {"type": "string", "description": "The live Echo task identifier."},
                        "status": {"type": "string", "description": "completed or failed."},
                        "summary": {"type": "string", "description": "A short result summary."}
                    },
                    "optional": {},
                    "timeout_ms": REPORT_TIMEOUT_MS
                }
            }
        }
    })
}

pub(super) fn register_ack(message: &Value) -> Option<bool> {
    let response = message["type"] == "res" && message["id"] == REGISTER_ID && message.get("method").is_none();
    response.then(|| message["ok"] == true)
}

pub(super) fn unpaired(message: &Value) -> bool {
    matches!(message["event"].as_str(), Some("link.unpaired" | "node.unpaired"))
}

pub(super) fn encode_result(mut value: Value) -> Result<Vec<u8>, InvalidWire> {
    if let Some(Value::String(output)) = value.get_mut("payload").and_then(|payload| payload.get_mut("output")) {
        let output = std::mem::take(output);
        value["payload"]["truncated"] = Value::Bool(false);
        let envelope_len = serde_json::to_vec(&value).map_err(|_| InvalidWire)?.len() + PREFIX;
        let budget = MAX_OUTPUT.min(MAX_CONTROL.checked_sub(envelope_len).ok_or(InvalidWire)?);
        let kept = output_prefix(&output, budget);
        value["payload"]["truncated"] = Value::Bool(kept.len() != output.len());
        value["payload"]["output"] = Value::String(kept.to_owned());
    }
    encode(&value)
}

fn output_prefix(output: &str, mut budget: usize) -> &str {
    for (offset, ch) in output.char_indices() {
        // Match serde_json's UTF-8 string encoding, excluding the surrounding quotes.
        let encoded = match ch {
            '"' | '\\' | '\u{8}' | '\t' | '\n' | '\u{c}' | '\r' => 2,
            '\u{0}'..='\u{1f}' => 6,
            _ => ch.len_utf8(),
        };
        if encoded > budget {
            return &output[..offset];
        }
        budget -= encoded;
    }
    output
}

pub(super) fn encode(value: &Value) -> Result<Vec<u8>, InvalidWire> {
    let json = serde_json::to_vec(value).map_err(|_| InvalidWire)?;
    if !value.is_object() || json.len() > MAX_CONTROL - PREFIX {
        return Err(InvalidWire);
    }
    let length = u32::try_from(json.len()).map_err(|_| InvalidWire)?;
    let mut bytes = Vec::with_capacity(PREFIX + json.len());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend(json);
    Ok(bytes)
}

#[derive(Default)]
pub(super) struct Decoder {
    pending: Vec<u8>,
}

impl Decoder {
    pub(super) fn next(&mut self, source: &mut &[u8]) -> Result<Option<Value>, InvalidWire> {
        self.fill(source, PREFIX);
        let Some(prefix) = self.pending.first_chunk::<PREFIX>() else {
            return Ok(None);
        };
        let length = usize::try_from(u32::from_le_bytes(*prefix)).map_err(|_| InvalidWire)?;
        if length == 0 || length > MAX_CONTROL - PREFIX {
            return Err(InvalidWire);
        }
        let frame_len = length + PREFIX;
        self.fill(source, frame_len);
        if self.pending.len() != frame_len {
            return Ok(None);
        }
        let json: Value = serde_json::from_slice(&self.pending[PREFIX..]).map_err(|_| InvalidWire)?;
        self.pending.clear();
        if !json.is_object() {
            return Err(InvalidWire);
        }
        Ok(Some(json))
    }

    fn fill(&mut self, source: &mut &[u8], limit: usize) {
        let count = limit.saturating_sub(self.pending.len()).min(source.len());
        let (appended, rest) = source.split_at(count);
        self.pending.extend_from_slice(appended);
        *source = rest;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t2_control_budget_and_fragmented_messages() {
        let cap = 256 * 1024;
        let short = encode(&json!({"a":1})).unwrap();
        assert_eq!(short, b"\x07\x00\x00\x00{\"a\":1}");
        let value = json!({"x": "a".repeat(cap - 12)});
        let bytes = encode(&value).unwrap();
        assert_eq!(bytes.len(), cap);
        let mut decoder = Decoder::default();
        let mut prefix = &bytes[..2];
        assert_eq!(decoder.next(&mut prefix), Ok(None));
        let mut tail = &bytes[2..];
        assert_eq!(decoder.next(&mut tail), Ok(Some(value)));
        assert_eq!(encode(&json!({"x": "a".repeat(cap - 11)})), Err(InvalidWire));
        let oversize = u32::try_from(cap - 3).unwrap().to_le_bytes();
        assert_eq!(decoder.next(&mut oversize.as_slice()), Err(InvalidWire));
    }
}
