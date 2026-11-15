//! Captured Wyoming framing and typed projection.
mod codec;
mod events;
pub use codec::{CodecError, Decoder, encode};
pub use events::{AudioChunkData, AudioFormat, RunPipelineData, WyomingEvent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Header {
    #[serde(rename = "type")]
    pub r#type: String,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present_value")]
    pub data: Option<Value>,
    #[serde(default)]
    pub data_length: usize,
    #[serde(default)]
    pub payload_length: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub header: Header,
    pub data: Option<Value>,
    pub payload: Vec<u8>,
}

fn present_value<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

impl Event {
    /// Project merged data, giving external object keys precedence over inline keys.
    /// # Errors
    /// Returns a serde error for malformed data of a recognized structured event.
    pub fn typed(&self) -> Result<WyomingEvent, serde_json::Error> {
        let mut data = self.header.data.clone().unwrap_or(Value::Null);
        if let Some(external) = &self.data {
            if let (Some(inline), Some(external)) = (data.as_object_mut(), external.as_object()) {
                inline.extend(external.clone());
            } else {
                data = external.clone();
            }
        }
        serde_json::from_value(serde_json::json!({"type": self.header.r#type, "data": data}))
    }
}
