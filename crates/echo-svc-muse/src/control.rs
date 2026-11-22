use serde_json::{Value, json};

use crate::wire::InvalidWire;

const MAX_CONTROL: usize = 256 * 1024;
const PREFIX: usize = size_of::<u32>();
const REGISTER_ID: &str = "reg-1";

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
                "echo.report.result": {
                    "description": "Report the result of a live Echo task. This is not user consent.",
                    "required": {
                        "task_id": {"type": "string", "description": "The live Echo task identifier."},
                        "status": {"type": "string", "description": "completed or failed."},
                        "summary": {"type": "string", "description": "A short result summary."}
                    },
                    "optional": {},
                    "timeout_ms": 15000
                }
            }
        }
    })
}

pub(super) fn register_ack(message: &Value) -> Option<bool> {
    let response = message["type"] == "res" && message["id"] == REGISTER_ID && message.get("method").is_none();
    response.then(|| message["ok"] == true)
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
