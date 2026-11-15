//! Detection, transcript, voice-stopped and error are requested but uncaptured.
//! Their data remains opaque; all other named tags are present in host captures.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioFormat {
    pub rate: u32,
    pub width: u16,
    pub channels: u16,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioChunkData {
    pub rate: u32,
    pub width: u16,
    pub channels: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunPipelineData {
    pub start_stage: String,
    pub end_stage: String,
    pub restart_on_end: bool,
    pub wake_word_name: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WyomingEvent {
    Describe(Value),
    Info(Value),
    RunSatellite(Value),
    PauseSatellite(Value),
    RunPipeline(RunPipelineData),
    AudioStart(AudioFormat),
    AudioChunk(AudioChunkData),
    AudioStop(Value),
    Played(Value),
    Detection(Value),
    Transcript(Value),
    VoiceStopped(Value),
    Error(Value),
    Ping(Value),
    Pong(Value),
    Other(String, Value),
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    data: Value,
}

impl WyomingEvent {
    fn from_envelope(value: Envelope) -> Result<Self, serde_json::Error> {
        Ok(match value.kind.as_str() {
            "describe" => Self::Describe(value.data),
            "info" => Self::Info(value.data),
            "run-satellite" => Self::RunSatellite(value.data),
            "pause-satellite" => Self::PauseSatellite(value.data),
            "run-pipeline" => Self::RunPipeline(serde_json::from_value(value.data)?),
            "audio-start" => Self::AudioStart(serde_json::from_value(value.data)?),
            "audio-chunk" => Self::AudioChunk(serde_json::from_value(value.data)?),
            "audio-stop" => Self::AudioStop(value.data),
            "played" => Self::Played(value.data),
            "detection" => Self::Detection(value.data),
            "transcript" => Self::Transcript(value.data),
            "voice-stopped" => Self::VoiceStopped(value.data),
            "error" => Self::Error(value.data),
            "ping" => Self::Ping(value.data),
            "pong" => Self::Pong(value.data),
            _ => Self::Other(value.kind, value.data),
        })
    }

    fn envelope(&self) -> Result<Envelope, serde_json::Error> {
        let (kind, data) = match self {
            Self::Describe(data) => ("describe", data.clone()),
            Self::Info(data) => ("info", data.clone()),
            Self::RunSatellite(data) => ("run-satellite", data.clone()),
            Self::PauseSatellite(data) => ("pause-satellite", data.clone()),
            Self::RunPipeline(data) => ("run-pipeline", serde_json::to_value(data)?),
            Self::AudioStart(data) => ("audio-start", serde_json::to_value(data)?),
            Self::AudioChunk(data) => ("audio-chunk", serde_json::to_value(data)?),
            Self::AudioStop(data) => ("audio-stop", data.clone()),
            Self::Played(data) => ("played", data.clone()),
            Self::Detection(data) => ("detection", data.clone()),
            Self::Transcript(data) => ("transcript", data.clone()),
            Self::VoiceStopped(data) => ("voice-stopped", data.clone()),
            Self::Error(data) => ("error", data.clone()),
            Self::Ping(data) => ("ping", data.clone()),
            Self::Pong(data) => ("pong", data.clone()),
            Self::Other(kind, data) => (kind.as_str(), data.clone()),
        };
        Ok(Envelope {
            kind: kind.into(),
            data,
        })
    }
}

impl Serialize for WyomingEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.envelope()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for WyomingEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_envelope(Envelope::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for WyomingEvent {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "WyomingEvent".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let structured = [
            ("run-pipeline", generator.subschema_for::<RunPipelineData>()),
            ("audio-start", generator.subschema_for::<AudioFormat>()),
            ("audio-chunk", generator.subschema_for::<AudioChunkData>()),
        ];
        let mut schema = generator.subschema_for::<Envelope>();
        let rules: Vec<_> = structured
            .into_iter()
            .map(|(tag, data)| {
                serde_json::json!({
                    "if": {"properties": {"type": {"const": tag}}, "required": ["type"]},
                    "then": {"properties": {"data": data}, "required": ["data"]}
                })
            })
            .collect();
        schema.insert("allOf".into(), rules.into());
        schema
    }
}
