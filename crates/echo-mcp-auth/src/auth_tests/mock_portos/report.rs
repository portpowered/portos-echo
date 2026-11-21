//! Strict `report-state` validation, after the live plugin's report decoder:
//! unknown fields refused, one reply per `requestId`, fresh sample times.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::Value;

const REPORT_NAMESPACE: &str = "port1/systems/zero/plugins/echo-show";
const SKEW_MS: i64 = 300_000;
const MAX_SNAPSHOT: usize = 4 * 1024 * 1024;
const MAX_SESSION_ID: usize = 128;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    header: Header,
    target: Target,
    payload: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    namespace: String,
    name: String,
    version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    #[serde(rename = "type")]
    kind: String,
    value: String,
}

pub(super) fn envelope(body: &Value, device: &str, now_ms: i64) -> Result<(), &'static str> {
    let message = serde_json::from_value::<Envelope>(body.clone()).map_err(|_| "envelope fields")?;
    let header = &message.header;
    if (header.namespace.as_str(), header.name.as_str(), header.version.as_str())
        != (REPORT_NAMESPACE, "report-state", "1.0")
    {
        return Err("header");
    }
    if message.target.kind != "ROUTE" || message.target.value != device {
        return Err("target is not the paired deviceId");
    }
    let report = serde_json::from_value::<Report>(message.payload).map_err(|_| "payload fields")?;
    report.validate(now_ms)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Report {
    sampled_at_ms: Option<i64>,
    speaker: Option<Speaker>,
    assistant: Option<Assistant>,
    #[serde(default)]
    request_id: String,
    snapshot: Option<Snapshot>,
    start_session: Option<StartSession>,
    end_session: Option<EndSession>,
    error: Option<Error>,
    session_ended: Option<SessionEnded>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Speaker {
    volume: Option<i64>,
    muted: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Assistant {
    state: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    origin: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    width: Option<i64>,
    height: Option<i64>,
    data: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StartSession {
    session_id: String,
    state: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndSession {
    state: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Error {
    code: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    origin: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SessionEnded {
    session_id: String,
    origin: String,
    reason: String,
    ended_at: String,
}

fn origin(text: &str) -> bool {
    matches!(text, "WAKE" | "REMOTE")
}

fn session_id(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_SESSION_ID
}

impl Report {
    fn replies(&self) -> usize {
        usize::from(self.snapshot.is_some())
            + usize::from(self.start_session.is_some())
            + usize::from(self.end_session.is_some())
            + usize::from(self.error.is_some())
    }

    fn shape(&self) -> Result<(), &'static str> {
        let replies = self.replies();
        let state = self.speaker.is_some() || self.assistant.is_some();
        if replies > 1 {
            return Err("more than one reply");
        }
        if (replies == 1) == self.request_id.is_empty() {
            return Err("requestId goes with exactly one reply");
        }
        if !state && replies == 0 && self.session_ended.is_none() {
            return Err("empty report");
        }
        if state && self.sampled_at_ms.is_none() {
            return Err("state without sampledAtMs");
        }
        Ok(())
    }

    fn validate(&self, now_ms: i64) -> Result<(), &'static str> {
        self.shape()?;
        if self.sampled_at_ms.is_some_and(|at| (at - now_ms).abs() > SKEW_MS) {
            return Err("sampledAtMs outside 300 s");
        }
        if let Some(speaker) = &self.speaker
            && (speaker.volume.is_none_or(|volume| !(0..=100).contains(&volume)) || speaker.muted.is_none())
        {
            return Err("speaker needs volume 0..100 and muted");
        }
        self.assistant.as_ref().map_or(Ok(()), Assistant::validate)?;
        self.validate_reply()?;
        self.session_ended
            .as_ref()
            .map_or(Ok(()), |ended| ended.validate(now_ms))
    }

    fn validate_reply(&self) -> Result<(), &'static str> {
        if let Some(snapshot) = &self.snapshot {
            snapshot.validate()?;
        }
        if let Some(start) = &self.start_session
            && (!matches!(start.state.as_str(), "LISTENING" | "THINKING" | "SPEAKING")
                || !session_id(&start.session_id))
        {
            return Err("startSession");
        }
        if let Some(end) = &self.end_session
            && !matches!(end.state.as_str(), "IDLE" | "MUTED")
        {
            return Err("endSession state");
        }
        self.error.as_ref().map_or(Ok(()), Error::validate)
    }
}

impl Assistant {
    fn validate(&self) -> Result<(), &'static str> {
        let active = match self.state.as_str() {
            "LISTENING" | "THINKING" | "SPEAKING" => true,
            "IDLE" | "MUTED" => false,
            _ => return Err("assistant state"),
        };
        if active && !(session_id(&self.session_id) && origin(&self.origin)) {
            return Err("active assistant needs sessionId and origin");
        }
        if !active && (!self.session_id.is_empty() || !self.origin.is_empty()) {
            return Err("idle assistant carries a session");
        }
        Ok(())
    }
}

impl Snapshot {
    fn validate(&self) -> Result<(), &'static str> {
        let data = STANDARD.decode(&self.data).map_err(|_| "snapshot base64")?;
        let dimension = |value: Option<i64>| value.is_some_and(|value| (1..=16_384).contains(&value));
        if data.is_empty() || data.len() > MAX_SNAPSHOT || !data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Err("snapshot must be a JPEG within 4 MiB");
        }
        if !dimension(self.width) || !dimension(self.height) {
            return Err("snapshot dimensions");
        }
        Ok(())
    }
}

impl Error {
    fn validate(&self) -> Result<(), &'static str> {
        let mut code = self.code.chars();
        let upper = code.next().is_some_and(|first| first.is_ascii_uppercase())
            && self.code.len() <= 64
            && code.all(|next| next.is_ascii_uppercase() || next.is_ascii_digit() || next == '_');
        if !upper {
            return Err("error code");
        }
        let busy = self.code == "BUSY";
        let session = !self.session_id.is_empty() || !self.origin.is_empty();
        if busy && !(session_id(&self.session_id) && origin(&self.origin)) || !busy && session {
            return Err("error session fields only with BUSY");
        }
        Ok(())
    }
}

impl SessionEnded {
    fn validate(&self, now_ms: i64) -> Result<(), &'static str> {
        if !matches!(
            self.reason.as_str(),
            "USER" | "TIMEOUT" | "MUTED" | "COMPLETED" | "REMOTE" | "ERROR"
        ) || !session_id(&self.session_id)
            || !origin(&self.origin)
        {
            return Err("sessionEnded");
        }
        let at = rfc3339_ms(&self.ended_at).ok_or("sessionEnded endedAt")?;
        if (at - now_ms).abs() > SKEW_MS {
            return Err("sessionEnded endedAt outside 300 s");
        }
        Ok(())
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.frac]Z` to epoch milliseconds (UTC only).
fn rfc3339_ms(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let (clock, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let mut clock = clock.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (clock.next()?.ok()?, clock.next()?.ok()?, clock.next()?.ok()?);
    let mut digits = fraction.to_owned();
    while digits.len() < 3 {
        digits.push('0');
    }
    let millis = digits.get(..3)?.parse::<i64>().ok()?;
    // Days from civil, proleptic Gregorian.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1000 + millis)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rfc3339_reads_utc_seconds_and_fractions() {
        assert_eq!(super::rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(super::rfc3339_ms("2027-01-15T08:00:00Z"), Some(1_800_000_000_000));
        assert_eq!(super::rfc3339_ms("2027-01-15T08:00:00.25Z"), Some(1_800_000_000_250));
        assert_eq!(super::rfc3339_ms("2027-01-15T08:00:00+01:00"), None); // lint-rules: allow-literal UTC offset fixture, not a host
    }
}
