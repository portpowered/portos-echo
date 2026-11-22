use super::{REQUEST_TIMEOUT, Transport};
use crate::bridge_endpoint::{
    commands::{self, Prepared},
    snapshot::CAPTURE_WINDOW,
};
use echo_mcp_auth::{EndpointTokenSource, new_request_id};
use reqwest::StatusCode;
use std::time::Duration;
use tokio::time::{Instant, sleep_until};

const MAX_MESSAGE: usize = 8 * 1024 * 1024;

impl Transport {
    pub(super) async fn send_reply(
        &self,
        tokens: &dyn EndpointTokenSource,
        reply: Prepared<Option<Vec<u8>>>,
        deadline: Instant,
    ) {
        match reply {
            Prepared::Reply(body) => {
                self.send(tokens, body, deadline).await;
            }
            Prepared::Speaker { body, code } => self.send_speaker_ack(tokens, body, code, deadline).await,
            Prepared::Snapshot {
                body,
                timeout,
                received,
            } => {
                let expires = received + CAPTURE_WINDOW;
                if self.send(tokens, body, expires).await {
                    commands::audit(self.trace.as_ref(), "get-snapshot", "OK", received);
                } else {
                    sleep_until(expires).await;
                    commands::audit(self.trace.as_ref(), "get-snapshot", "TIMEOUT", received);
                    self.send(tokens, timeout, deadline).await;
                }
            }
        }
    }

    /// Send one speaker ack. Port OS applies its state even when no caller
    /// waits any more (a fire-and-forget request, or one past its 60 s) and
    /// then answers 404: that ack is done, never retried.
    async fn send_speaker_ack(
        &self,
        tokens: &dyn EndpointTokenSource,
        body: Option<Vec<u8>>,
        code: &'static str,
        deadline: Instant,
    ) {
        let (outcome, http_status) = match self.deliver(tokens, body, deadline).await {
            Delivery::Answered => ("answered", None),
            Delivery::Unrouted => ("not_waiting", Some(StatusCode::NOT_FOUND.as_u16())),
            Delivery::Expired => ("expired", None),
        };
        let fields = serde_json::json!({ "outcome": outcome, "code": code, "http_status": http_status });
        crate::bridge_endpoint::note(self.trace.as_ref(), "endpoint.speaker_ack", fields);
    }

    /// Returns false when the deadline prevents this report's delivery or next retry.
    pub(super) async fn send(
        &self,
        tokens: &dyn EndpointTokenSource,
        body: Option<Vec<u8>>,
        deadline: Instant,
    ) -> bool {
        self.deliver(tokens, body, deadline).await != Delivery::Expired
    }

    /// Post one message, retrying unanswered requests and server errors until the deadline.
    pub(super) async fn deliver(
        &self,
        tokens: &dyn EndpointTokenSource,
        body: Option<Vec<u8>>,
        deadline: Instant,
    ) -> Delivery {
        let Some(body) = body.filter(|bytes| bytes.len() <= MAX_MESSAGE) else {
            return Delivery::Answered;
        };
        let Ok(key) = new_request_id() else {
            return Delivery::Answered;
        };
        while Instant::now() < deadline {
            // `Err` retries; `Ok(None)` stops without an answer.
            let attempt = async {
                let Ok(token) = tokens.access_token().await else {
                    return Ok(None);
                };
                let request = self
                    .http
                    .post(self.messages.clone())
                    .bearer_auth(token.expose())
                    .header("Idempotency-Key", &key)
                    .header("Content-Type", "application/json")
                    .body(body.clone());
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(None);
                }
                match request.timeout(remaining.min(REQUEST_TIMEOUT)).send().await {
                    Ok(reply) if !reply.status().is_server_error() => Ok(Some(reply.status())),
                    _ => Err(()),
                }
            };
            let answer = tokio::select! {
                biased;
                () = sleep_until(deadline) => return Delivery::Expired,
                answer = attempt => answer,
            };
            if let Ok(status) = answer {
                return if Instant::now() >= deadline {
                    Delivery::Expired
                } else if status == Some(StatusCode::NOT_FOUND) {
                    Delivery::Unrouted
                } else {
                    Delivery::Answered
                };
            }
            let retry = Instant::now() + Duration::from_millis(250);
            if retry >= deadline {
                return Delivery::Expired;
            }
            sleep_until(retry).await;
        }
        Delivery::Expired
    }
}

/// How one uplink message ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Delivery {
    /// Port OS answered it, or it cannot be sent at all.
    Answered,
    /// Port OS answered 404: for state, no route for this device yet, so send it
    /// again once one appears; for a reply, no command is waiting for it.
    Unrouted,
    /// The deadline passed first.
    Expired,
}
