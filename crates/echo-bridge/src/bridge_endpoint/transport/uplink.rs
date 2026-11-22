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
    /// Port OS has no route for this device yet (404); send the state again once one appears.
    Unrouted,
    /// The deadline passed first.
    Expired,
}
