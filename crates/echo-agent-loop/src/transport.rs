use crate::{InitError, ModelCredential, wire::Decoder};
use echo_config::LiveMode;
use reqwest::{
    Client, Url,
    header::{ACCEPT, AUTHORIZATION, HeaderMap},
};
use serde_json::Value;

#[derive(Clone)]
pub(crate) struct Transport {
    client: Client,
    url: Url,
    mode: LiveMode,
}

pub(crate) fn production_url(mode: LiveMode) -> &'static str {
    match mode {
        LiveMode::Chatgpt => "https://chatgpt.com/backend-api/codex/responses",
        LiveMode::ApiKey => "https://api.openai.com/v1/responses",
    }
}

impl Transport {
    pub(crate) fn new(mode: LiveMode, base: Option<&str>) -> Result<Self, InitError> {
        let mut url = Url::parse(production_url(mode)).map_err(|_| InitError::Endpoint)?;
        if let Some(base) = base {
            let base = Url::parse(base).map_err(|_| InitError::Endpoint)?;
            if base.scheme() != "http"
                || base.host_str() != Some("127.0.0.1")
                || !base.username().is_empty()
                || base.password().is_some()
                || base.query().is_some()
                || base.fragment().is_some()
                || base.path() != "/"
            {
                return Err(InitError::Endpoint);
            }
            url = base.join(url.path()).map_err(|_| InitError::Endpoint)?;
        }
        let client = Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| InitError::Client)?;
        Ok(Self { client, url, mode })
    }

    pub(crate) async fn request<C: ModelCredential>(
        &self,
        credential: &C,
        body: &Value,
        budget: &crate::budget::Budget,
    ) -> Result<Vec<Value>, &'static str> {
        let secret = credential
            .headers(self.mode)
            .await
            .map_err(|_| "model credential unavailable")?;
        let headers = validated_headers(secret.expose(), self.mode)?;
        budget.reserve()?;
        let mut response = self
            .client
            .post(self.url.clone())
            .headers(headers)
            .json(body)
            .send()
            .await
            .map_err(|_| "model request failed")?;
        if !response.status().is_success() {
            return Err("model request failed");
        }
        let mut decoder = Decoder::default();
        while let Some(chunk) = response.chunk().await.map_err(|_| "invalid model response")? {
            decoder.push(&chunk)?;
        }
        decoder.finish()
    }
}

fn validated_headers(source: &HeaderMap, mode: LiveMode) -> Result<HeaderMap, &'static str> {
    let mut headers = source.clone();
    let authorization = headers.get(AUTHORIZATION).and_then(|value| value.to_str().ok());
    if !authorization.is_some_and(|value| {
        value
            .strip_prefix("Bearer ")
            .is_some_and(|token| !token.trim().is_empty())
    }) {
        return Err("model credential unavailable");
    }
    if mode == LiveMode::Chatgpt {
        if headers
            .get("chatgpt-account-id")
            .is_none_or(|value| value.as_bytes().is_empty())
        {
            return Err("model credential unavailable");
        }
    } else {
        headers.remove("chatgpt-account-id");
    }
    for value in headers.values_mut() {
        value.set_sensitive(true);
    }
    headers.insert(
        ACCEPT,
        "text/event-stream"
            .parse()
            .map_err(|_| "model credential unavailable")?,
    );
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_mapping_and_loopback_restriction() {
        assert_eq!(
            production_url(LiveMode::Chatgpt),
            "https://chatgpt.com/backend-api/codex/responses" // lint-rules: allow-literal offline production URL assertion, never connected
        );
        assert_eq!(production_url(LiveMode::ApiKey), "https://api.openai.com/v1/responses"); // lint-rules: allow-literal offline production URL assertion, never connected
        for base in [
            "https://127.0.0.1",
            "http://localhost", // lint-rules: allow-literal endpoint rejection fixture, no connection
            "http://192.0.2.1", // lint-rules: allow-literal endpoint rejection fixture, no connection
            "http://127.0.0.1/path",
            "http://127.0.0.1/?query",
            "http://x@127.0.0.1",
        ] {
            assert!(Transport::new(LiveMode::ApiKey, Some(base)).is_err());
        }
        let transport = Transport::new(LiveMode::Chatgpt, Some("http://127.0.0.1:1234")).unwrap();
        assert_eq!(
            transport.url.as_str(),
            "http://127.0.0.1:1234/backend-api/codex/responses"
        );
    }

    #[test]
    fn credential_validation_is_safe_and_marks_sensitive() {
        let mut headers = HeaderMap::new();
        for value in ["", "Basic synthetic", "Bearer "] {
            headers.insert(AUTHORIZATION, value.parse().unwrap());
            assert_eq!(
                validated_headers(&headers, LiveMode::ApiKey).unwrap_err(),
                "model credential unavailable"
            );
        }
        headers.insert(AUTHORIZATION, "Bearer synthetic".parse().unwrap());
        assert!(validated_headers(&headers, LiveMode::Chatgpt).is_err());
        headers.insert("chatgpt-account-id", "".parse().unwrap());
        assert!(validated_headers(&headers, LiveMode::Chatgpt).is_err());
        headers.insert("chatgpt-account-id", "synthetic".parse().unwrap());
        let chatgpt = validated_headers(&headers, LiveMode::Chatgpt).unwrap();
        assert!(chatgpt[AUTHORIZATION].is_sensitive());
        assert!(chatgpt["chatgpt-account-id"].is_sensitive());
        assert!(
            !validated_headers(&headers, LiveMode::ApiKey)
                .unwrap()
                .contains_key("chatgpt-account-id")
        );
    }
}
