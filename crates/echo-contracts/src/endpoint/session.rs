use super::{AssistantState, MAX_SESSION_ID, SessionOrigin};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartSession {
    #[serde(default, deserialize_with = "initial_text")]
    pub initial_text: Option<String>,
    #[serde(default = "listen_by_default")]
    pub listen: bool,
    #[serde(deserialize_with = "reason")]
    pub reason: String,
}

fn initial_text<'de, D: Deserializer<'de>>(data: D) -> Result<Option<String>, D::Error> {
    let text = String::deserialize(data)?;
    let length = text.chars().count();
    if length == 0 || length > 2_000 {
        return Err(D::Error::custom("initialText must be 1 to 2000 characters"));
    }
    Ok(Some(text))
}

fn reason<'de, D: Deserializer<'de>>(data: D) -> Result<String, D::Error> {
    let text = String::deserialize(data)?;
    let length = text.chars().count();
    if length == 0 || length > 200 {
        return Err(D::Error::custom("reason must be 1 to 200 characters"));
    }
    Ok(text)
}

fn listen_by_default() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndSession {
    pub session_id: String,
}

impl EndSession {
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.session_id.trim().is_empty() && self.session_id.chars().count() <= MAX_SESSION_ID
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartSessionReply {
    pub session_id: String,
    pub state: AssistantState,
}

impl StartSessionReply {
    #[must_use]
    pub fn admitted(session_id: &str, state: AssistantState) -> Self {
        Self {
            session_id: session_id.into(),
            state,
        }
    }
}

/// The state after end-session: IDLE unless the microphone latch is set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EndSessionReply {
    pub state: AssistantState,
}

impl EndSessionReply {
    #[must_use]
    pub fn closed(muted: bool) -> Self {
        Self {
            state: if muted {
                AssistantState::Muted
            } else {
                AssistantState::Idle
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnded {
    pub session_id: String,
    pub origin: SessionOrigin,
    pub reason: EndedReason,
    /// RFC 3339, within 300 s of Port OS time.
    pub ended_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum EndedReason {
    User,
    Timeout,
    Muted,
    Completed,
    Remote,
    Error,
}
