//! PROVISIONAL until M2 replaces these shapes with captured GPT-Live traces.
use crate::ids::DelegationId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DelegationTarget {
    Client,
    Responses,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Delegation {
    pub id: DelegationId,
    pub target: DelegationTarget,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// PROVISIONAL vocabulary; representing Responses does not authorize execution.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveEvent {
    DelegationCreated {
        delegation: Delegation,
        extra: BTreeMap<String, Value>,
    },
    CommentaryAppend {
        delegation_id: DelegationId,
        text: String,
        extra: BTreeMap<String, Value>,
    },
    SessionCreated {
        extra: BTreeMap<String, Value>,
    },
    Error {
        extra: BTreeMap<String, Value>,
    },
    Other(Value),
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
enum Known {
    #[serde(rename = "session.delegation.created")]
    DelegationCreated {
        delegation: Delegation,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    #[serde(rename = "session.commentary.append")]
    CommentaryAppend {
        delegation_id: DelegationId,
        text: String,
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    #[serde(rename = "session.created")]
    SessionCreated {
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(flatten)]
        extra: BTreeMap<String, Value>,
    },
}

impl From<Known> for LiveEvent {
    fn from(value: Known) -> Self {
        match value {
            Known::DelegationCreated { delegation, extra } => Self::DelegationCreated { delegation, extra },
            Known::CommentaryAppend {
                delegation_id,
                text,
                extra,
            } => Self::CommentaryAppend {
                delegation_id,
                text,
                extra,
            },
            Known::SessionCreated { extra } => Self::SessionCreated { extra },
            Known::Error { extra } => Self::Error { extra },
        }
    }
}
impl Serialize for LiveEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let known = match self {
            Self::DelegationCreated { delegation, extra } => Known::DelegationCreated {
                delegation: delegation.clone(),
                extra: extra.clone(),
            },
            Self::CommentaryAppend {
                delegation_id,
                text,
                extra,
            } => Known::CommentaryAppend {
                delegation_id: delegation_id.clone(),
                text: text.clone(),
                extra: extra.clone(),
            },
            Self::SessionCreated { extra } => Known::SessionCreated { extra: extra.clone() },
            Self::Error { extra } => Known::Error { extra: extra.clone() },
            Self::Other(value) => return value.serialize(serializer),
        };
        known.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for LiveEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match value.get("type").and_then(Value::as_str) {
            Some("session.delegation.created" | "session.commentary.append" | "session.created" | "error") => {
                serde_json::from_value::<Known>(value)
                    .map(Into::into)
                    .map_err(serde::de::Error::custom)
            }
            _ => Ok(Self::Other(value)),
        }
    }
}
impl JsonSchema for LiveEvent {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LiveEvent".into()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"anyOf": [generator.subschema_for::<Known>(), {
            "not": {"type": "object", "required": ["type"], "properties": {"type": {
                "enum": ["session.delegation.created", "session.commentary.append", "session.created", "error"]
            }}}
        }]})
    }
}
