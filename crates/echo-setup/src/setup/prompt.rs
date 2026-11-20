use crate::{Failure, Result};
use echo_screen_contract::{ScreenFrame, UiState};
use reqwest::Url;
use serde::Serialize;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Chatgpt,
    Portos,
}

/// Only explicitly public code/URL fields can be serialized. Neither arbitrary
/// screen messages nor pairing credentials implement a presentation path here.
#[derive(PartialEq, Eq, Serialize)]
pub struct Prompt {
    #[serde(rename = "type")]
    kind: &'static str,
    provider: Provider,
    user_code: String,
    verification_uri: String,
    #[serde(skip)]
    expires_at_unix: Option<u64>,
}

pub(super) fn project(frame: ScreenFrame, provider: Provider, now: u64) -> Result<Option<Prompt>> {
    let mut expiry = None;
    let fields = match (provider, frame.state) {
        (Provider::Chatgpt, UiState::Ready { message }) => {
            // echo-signin::Prompt::message is the existing presentation contract.
            let Some(rest) = message.strip_prefix("code: ") else {
                return Ok(None);
            };
            let (code, url) = rest.split_once("\nurl: ").ok_or(Failure("setup_screen_invalid"))?;
            let url = url.strip_suffix('\n').ok_or(Failure("setup_screen_invalid"))?;
            Some((code.to_owned(), url.to_owned()))
        }
        (
            Provider::Portos,
            UiState::LinkCode {
                user_code,
                verification_uri,
                expires_at_unix,
            },
        ) => {
            if expires_at_unix <= now {
                return Err(Failure("setup_code_expired"));
            }
            expiry = Some(expires_at_unix);
            Some((user_code, verification_uri))
        }
        (_, UiState::Error { .. }) => return Err(Failure("setup_provider_failed")),
        _ => None,
    };
    fields
        .map(|(code, url)| validated(provider, code, url, expiry))
        .transpose()
}

impl Prompt {
    pub(super) fn expiry(&self, started: tokio::time::Instant, now_unix: u64) -> Option<tokio::time::Instant> {
        started.checked_add(std::time::Duration::from_secs(
            self.expires_at_unix?.saturating_sub(now_unix),
        ))
    }
}

fn validated(provider: Provider, code: String, url: String, expires_at_unix: Option<u64>) -> Result<Prompt> {
    // Device user codes are printable identifiers, never terminal control input.
    if code.is_empty() || code.len() > 128 || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(Failure("setup_screen_invalid"));
    }
    let parsed = Url::parse(&url).map_err(|_| Failure("setup_screen_invalid"))?;
    let host = match provider {
        Provider::Chatgpt => "auth.openai.com",
        Provider::Portos => "auth.portoperatingsystem.com",
    };
    if url.len() > 2048
        || url.chars().any(char::is_control)
        || parsed.as_str() != url
        || parsed.scheme() != "https"
        || parsed.host_str() != Some(host)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || (provider == Provider::Chatgpt && parsed.path() != "/codex/device")
        || (provider == Provider::Portos && parsed.path() != "/device")
    {
        return Err(Failure("setup_screen_invalid"));
    }
    Ok(Prompt {
        kind: "prompt",
        provider,
        user_code: code,
        verification_uri: url,
        expires_at_unix,
    })
}
