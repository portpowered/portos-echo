use super::{REQUEST_TIMEOUT, Transport};
use crate::bridge_endpoint::{
    commands::{self, Prepared},
    snapshot::CAPTURE_WINDOW,
};
use echo_mcp_auth::{EndpointTokenSource, new_request_id};
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
        let Some(body) = body.filter(|bytes| bytes.len() <= MAX_MESSAGE) else {
            return true;
        };
        let Ok(key) = new_request_id() else { return true };
        while Instant::now() < deadline {
            let attempt = async {
                let Ok(token) = tokens.access_token().await else {
                    return false;
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
                    return false;
                }
                request
                    .timeout(remaining.min(REQUEST_TIMEOUT))
                    .send()
                    .await
                    .map_or(true, |reply| reply.status().is_server_error())
            };
            let retry = tokio::select! {
                biased;
                () = sleep_until(deadline) => return false,
                retry = attempt => retry,
            };
            if !retry {
                return Instant::now() < deadline;
            }
            let retry = Instant::now() + Duration::from_millis(250);
            if retry >= deadline {
                return false;
            }
            sleep_until(retry).await;
        }
        false
    }
}
