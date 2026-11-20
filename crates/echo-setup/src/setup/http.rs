use super::{Prompt, Provider, events::Events};
use crate::{Failure, Result};
use echo_contracts::secret::Secret;
use reqwest::{Client, Response, Url};
use serde::Deserialize;
use std::time::Duration;
use tokio::time::Instant;

/// Observations of existing /healthz fields, never account or token data.
#[derive(Deserialize, PartialEq, Eq)]
pub struct Health {
    status: String,
    wyoming: String,
    live: String,
    pub relink_required: bool,
    pub kill_switch: bool,
}

impl Health {
    /// Runtime admission signals only; Port OS approval has its own owner.
    #[must_use]
    pub fn runtime_ready(&self) -> bool {
        self.status == "running" && self.wyoming == "connected" && !self.relink_required && !self.kill_switch
    }

    /// An active Live observation alone does not prove a fresh wake or run.
    #[must_use]
    pub fn live_connected(&self) -> bool {
        self.runtime_ready() && self.live == "connected"
    }
}

/// Holds only this SSE response. Dropping it cancels pending reads; it cannot
/// revoke a pairing token, change the device or remove someone else's forward.
pub struct ScreenEvents {
    response: Response,
    events: Events,
    previous: Option<Prompt>,
    deadline: Instant,
}

impl ScreenEvents {
    /// Connect only to a locally allocated adb-forward port. Never follow a
    /// redirect carrying the secret query to any other endpoint.
    /// # Errors
    /// Fixed categories for HTTP, pairing, protocol or deadline failures.
    pub async fn connect(port: u16, pairing: &Secret<String>, deadline: Instant) -> Result<Self> {
        validate_pairing(pairing)?;
        let mut url = endpoint(port, "/events")?;
        url.query_pairs_mut().append_pair("token", pairing.expose());
        let response = request(url, deadline).await?;
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next());
        if mime != Some("text/event-stream") {
            return Err(Failure("setup_screen_invalid"));
        }
        Ok(Self {
            response,
            events: Events::default(),
            previous: None,
            deadline,
        })
    }

    /// Wait for a new public provider prompt, without treating stream EOF or
    /// unrelated screen activity as successful authentication.
    /// # Errors
    /// Fixed resumable errors; no dependency text or URLs in diagnostics.
    pub async fn next_prompt(&mut self, provider: Provider, now_unix: u64) -> Result<Prompt> {
        let deadline = self.deadline;
        let started = Instant::now();
        tokio::time::timeout_at(deadline, async {
            loop {
                if Instant::now() >= deadline {
                    return Err(Failure("setup_timeout"));
                }
                let now = now_unix.saturating_add(started.elapsed().as_secs());
                if let Some(prompt) = self.events.next(provider, now)? {
                    if self.previous.as_ref() != Some(&prompt) {
                        return Ok(prompt);
                    }
                } else {
                    let bytes = self
                        .response
                        .chunk()
                        .await
                        .map_err(|_| Failure("setup_observation_failed"))?
                        .ok_or(Failure("setup_screen_closed"))?;
                    self.events.push(&bytes)?;
                }
            }
        })
        .await
        .map_err(|_| Failure("setup_timeout"))?
    }

    /// Acknowledge presentation so replayed identical codes do not spam output.
    pub fn presented(&mut self, prompt: Prompt) {
        self.previous = Some(prompt);
    }
}

/// # Errors
/// Fixed HTTP/schema/deadline categories. Response size is bounded.
pub async fn health(port: u16, deadline: Instant) -> Result<Health> {
    json_response(endpoint(port, "/healthz")?, deadline, 16_384, "setup_health_invalid").await
}

/// Read only the existing authenticated settings projection. An unavailable or
/// malformed observation never authorizes replacing an existing grant.
/// # Errors
/// Fixed pairing, HTTP, settings, unconfigured or absolute deadline categories.
pub async fn link_required(port: u16, pairing: &Secret<String>, deadline: Instant) -> Result<bool> {
    validate_pairing(pairing)?;
    let mut url = endpoint(port, "/settings/v1")?;
    url.query_pairs_mut().append_pair("token", pairing.expose());
    let snapshot: LinkSnapshot = json_response(url, deadline, 262_144, "setup_settings_invalid").await?;
    if snapshot.version != 1 {
        return Err(Failure("setup_settings_invalid"));
    }
    match snapshot.link_status {
        LinkStatus::Linked => Ok(false),
        LinkStatus::LinkRequired => Ok(true),
        LinkStatus::Unconfigured => Err(Failure("setup_link_unconfigured")),
    }
}

#[derive(Deserialize)]
struct LinkSnapshot {
    version: u8,
    link_status: LinkStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LinkStatus {
    Unconfigured,
    LinkRequired,
    Linked,
}

fn validate_pairing(pairing: &Secret<String>) -> Result<()> {
    if pairing.expose().trim().is_empty()
        || pairing.expose().len() > 16_384
        || pairing.expose().chars().any(char::is_control)
    {
        return Err(Failure("setup_pairing_invalid"));
    }
    Ok(())
}

async fn json_response<T: serde::de::DeserializeOwned>(
    url: Url,
    deadline: Instant,
    limit: usize,
    invalid: &'static str,
) -> Result<T> {
    tokio::time::timeout_at(deadline, async {
        let mut response = request(url, deadline).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Failure("setup_observation_failed"))?
        {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(Failure(invalid));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| Failure(invalid))
    })
    .await
    .map_err(|_| Failure("setup_timeout"))?
}

fn endpoint(port: u16, path: &str) -> Result<Url> {
    if port == 0 {
        return Err(Failure("setup_forward_failed"));
    }
    let mut url = Url::parse("http://127.0.0.1").map_err(|_| Failure("setup_forward_failed"))?;
    url.set_port(Some(port)).map_err(|()| Failure("setup_forward_failed"))?;
    url.set_path(path);
    Ok(url)
}

async fn request(url: Url, deadline: Instant) -> Result<Response> {
    if Instant::now() >= deadline {
        return Err(Failure("setup_timeout"));
    }
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| Failure("setup_observation_failed"))?;
    let response = tokio::time::timeout_at(deadline, client.get(url).send())
        .await
        .map_err(|_| Failure("setup_timeout"))?
        .map_err(|_| Failure("setup_observation_failed"))?;
    if !response.status().is_success() {
        return Err(Failure("setup_observation_failed"));
    }
    Ok(response)
}
