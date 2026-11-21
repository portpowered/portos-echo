use super::{AssistantState, SessionOrigin};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartSession {
    #[serde(default, deserialize_with = "initial_text")]
    pub initial_text: Option<String>,
    #[serde(default = "listen_by_default")]
    pub listen: bool,
    pub reason: String,
}

fn initial_text<'de, D: Deserializer<'de>>(data: D) -> Result<Option<String>, D::Error> {
    let text = String::deserialize(data)?;
    if text.chars().count() > 2_000 {
        return Err(D::Error::custom("initialText exceeds 2000 characters"));
    }
    Ok(Some(text))
}

fn listen_by_default() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndSession {
    pub session_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartSessionOutcome<'a> {
    ok: bool,
    session_id: &'a str,
    state: AssistantState,
}

impl<'a> StartSessionOutcome<'a> {
    #[must_use]
    pub fn admitted(session_id: &'a str, state: AssistantState) -> Self {
        Self {
            ok: true,
            session_id,
            state,
        }
    }
}

#[derive(Serialize)]
pub struct EndSessionOutcome {
    ok: bool,
    state: AssistantState,
    ending: bool,
}

impl EndSessionOutcome {
    #[must_use]
    pub fn closed() -> Self {
        Self {
            ok: true,
            state: AssistantState::Idle,
            ending: false,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnded<'a> {
    pub session_id: &'a str,
    pub origin: SessionOrigin,
    pub reason: EndedReason,
    pub ended_at: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndedReason {
    User,
    Timeout,
    Muted,
    Completed,
    Remote,
    Error,
}
