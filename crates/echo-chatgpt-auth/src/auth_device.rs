use crate::{AuthError, AuthSource, DeviceCodePrompt, auth_http, auth_store::Record, auth_token};
use echo_contracts::secret::Secret;
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
struct Challenge {
    device_auth_id: Secret<String>,
    #[serde(alias = "usercode")]
    user_code: Secret<String>,
    #[serde(default)]
    interval: serde_json::Value,
}
#[derive(Deserialize)]
struct Approval {
    authorization_code: Secret<String>,
    code_verifier: Secret<String>,
}
fn interval(value: &serde_json::Value) -> Duration {
    let seconds = value.as_f64().or_else(|| value.as_str()?.parse().ok());
    seconds
        .filter(|s| s.is_finite() && *s > 0.0)
        .and_then(|s| Duration::try_from_secs_f64(s.max(1.0)).ok())
        .unwrap_or(Duration::from_secs(5))
}
fn json(source: &AuthSource, endpoint: &str) -> reqwest::RequestBuilder {
    source
        .client
        .post(endpoint)
        .header("originator", "openclaw")
        .header("User-Agent", "openclaw")
}
pub(crate) async fn sign_in(source: &AuthSource, callback: impl FnOnce(DeviceCodePrompt)) -> Result<Record, AuthError> {
    let request =
        json(source, &source.config.usercode_endpoint).json(&serde_json::json!({"client_id": source.config.client_id}));
    let (status, body) = auth_http::request(request, Duration::from_secs(30)).await?;
    if !status.is_success() {
        return Err(AuthError::AuthorizationDenied);
    }
    let device: Challenge = auth_http::decode(&body)?;
    auth_token::nonempty(device.device_auth_id.expose())?;
    auth_token::nonempty(device.user_code.expose())?;
    callback(DeviceCodePrompt {
        user_code: device.user_code.expose().clone(),
        verification_url: source.config.verification_url.clone(),
    });
    let approval = poll(source, &device).await?;
    exchange(source, &approval).await
}
async fn poll(source: &AuthSource, device: &Challenge) -> Result<Approval, AuthError> {
    let deadline = source.clock.monotonic() + Duration::from_mins(15);
    while source.clock.monotonic() < deadline {
        let remaining = deadline.saturating_sub(source.clock.monotonic());
        let request = json(source, &source.config.device_token_endpoint).json(&serde_json::json!({
            "device_auth_id": device.device_auth_id.expose(), "user_code": device.user_code.expose(),
        }));
        let (status, body) = auth_http::request(request, remaining.min(Duration::from_secs(30))).await?;
        if status.is_success() {
            let approval: Approval = auth_http::decode(&body)?;
            auth_token::nonempty(approval.authorization_code.expose())?;
            auth_token::nonempty(approval.code_verifier.expose())?;
            return Ok(approval);
        }
        if !matches!(status.as_u16(), 403 | 404) {
            return Err(AuthError::AuthorizationDenied);
        }
        source
            .clock
            .wait(interval(&device.interval).min(deadline.saturating_sub(source.clock.monotonic())))
            .await;
    }
    Err(AuthError::Timeout)
}
async fn exchange(source: &AuthSource, approval: &Approval) -> Result<Record, AuthError> {
    let request = json(source, &source.config.token_endpoint).form(&[
        ("grant_type", "authorization_code"),
        ("client_id", &source.config.client_id),
        ("code", approval.authorization_code.expose()),
        ("code_verifier", approval.code_verifier.expose()),
        ("redirect_uri", &source.config.device_redirect_uri),
    ]);
    let (status, body) = auth_http::request(request, Duration::from_secs(30)).await?;
    if !status.is_success() {
        return Err(AuthError::RelinkRequired);
    }
    auth_token::response(&body, source.clock.unix()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interval_defaults_clamps_and_rejects_overflow() {
        for value in [
            serde_json::Value::Null,
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!("NaN"),
            serde_json::json!("1e99"),
        ] {
            assert_eq!(interval(&value), Duration::from_secs(5));
        }
        for value in [serde_json::json!("5"), serde_json::json!(5)] {
            assert_eq!(interval(&value), Duration::from_secs(5));
        }
        assert_eq!(interval(&serde_json::json!(0.1)), Duration::from_secs(1));
    }
}
