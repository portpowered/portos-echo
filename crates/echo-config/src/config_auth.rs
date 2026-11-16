//! Plain, offline `ChatGPT` endpoint configuration; no credential discovery.
use crate::{ConfigError, config_decode::Fields, config_validate::http_url};

pub(super) const FIELDS: &[&str] = &[
    "issuer",
    "client_id",
    "scopes",
    "usercode_endpoint",
    "device_token_endpoint",
    "token_endpoint",
    "authorization_endpoint",
    "verification_url",
    "device_redirect_uri",
    "browser_redirect_uri",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptAuthConfig {
    pub issuer: String,
    pub client_id: String,
    pub scopes: String,
    pub usercode_endpoint: String,
    pub device_token_endpoint: String,
    pub token_endpoint: String,
    pub authorization_endpoint: String,
    pub verification_url: String,
    pub device_redirect_uri: String,
    pub browser_redirect_uri: String,
}

impl Default for ChatGptAuthConfig {
    fn default() -> Self {
        let issuer = "https://auth.openai.com";
        Self {
            issuer: issuer.into(),
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann".into(),
            scopes: "openid profile email offline_access".into(),
            usercode_endpoint: format!("{issuer}/api/accounts/deviceauth/usercode"),
            device_token_endpoint: format!("{issuer}/api/accounts/deviceauth/token"),
            token_endpoint: format!("{issuer}/oauth/token"),
            authorization_endpoint: format!("{issuer}/oauth/authorize"),
            verification_url: format!("{issuer}/codex/device"),
            device_redirect_uri: format!("{issuer}/deviceauth/callback"),
            browser_redirect_uri: "http://localhost:1455/auth/callback".into(),
        }
    }
}

pub(super) fn decode(value: toml::de::DeValue<'_>) -> Result<ChatGptAuthConfig, ConfigError> {
    let mut f = Fields::new(value, "chatgpt_auth")?;
    let d = ChatGptAuthConfig::default();
    let result = ChatGptAuthConfig {
        issuer: f.optional("issuer")?.unwrap_or(d.issuer),
        client_id: f.optional("client_id")?.unwrap_or(d.client_id),
        scopes: f.optional("scopes")?.unwrap_or(d.scopes),
        usercode_endpoint: f.optional("usercode_endpoint")?.unwrap_or(d.usercode_endpoint),
        device_token_endpoint: f.optional("device_token_endpoint")?.unwrap_or(d.device_token_endpoint),
        token_endpoint: f.optional("token_endpoint")?.unwrap_or(d.token_endpoint),
        authorization_endpoint: f
            .optional("authorization_endpoint")?
            .unwrap_or(d.authorization_endpoint),
        verification_url: f.optional("verification_url")?.unwrap_or(d.verification_url),
        device_redirect_uri: f.optional("device_redirect_uri")?.unwrap_or(d.device_redirect_uri),
        browser_redirect_uri: f.optional("browser_redirect_uri")?.unwrap_or(d.browser_redirect_uri),
    };
    f.finish()?;
    Ok(result)
}

fn authority(url: &str) -> &str {
    url.split_once("://")
        .map_or("", |(_, rest)| rest.split('/').next().unwrap_or_default())
}

fn endpoint(url: &str) -> bool {
    http_url(url)
        && !url.contains(['#', '?'])
        && (url.starts_with("https://")
            || (url.starts_with("http://")
                && (authority(url) == "127.0.0.1" || authority(url).starts_with("127.0.0.1:"))))
}

impl ChatGptAuthConfig {
    /// Revalidate public fields after mutation, without DNS or I/O.
    ///
    /// # Errors
    /// Returns a field-only diagnostic for an unsafe endpoint or empty client/scopes.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (key, value) in [
            ("issuer", &self.issuer),
            ("usercode_endpoint", &self.usercode_endpoint),
            ("device_token_endpoint", &self.device_token_endpoint),
            ("token_endpoint", &self.token_endpoint),
            ("authorization_endpoint", &self.authorization_endpoint),
            ("verification_url", &self.verification_url),
            ("device_redirect_uri", &self.device_redirect_uri),
        ] {
            if !endpoint(value) {
                return Err(ConfigError::new(
                    format!("chatgpt_auth.{key}"),
                    "expected HTTPS or literal loopback HTTP URL",
                ));
            }
        }
        for (key, value) in [("client_id", &self.client_id), ("scopes", &self.scopes)] {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                return Err(ConfigError::new(
                    format!("chatgpt_auth.{key}"),
                    "expected nonempty text",
                ));
            }
        }
        let redirect = &self.browser_redirect_uri;
        let host = authority(redirect).split(':').next().unwrap_or_default();
        if !http_url(redirect)
            || !redirect.starts_with("http://")
            || !["localhost", "127.0.0.1"].contains(&host)
            || redirect.splitn(4, '/').nth(3) != Some("auth/callback")
        {
            return Err(ConfigError::new(
                "chatgpt_auth.browser_redirect_uri",
                "expected loopback HTTP /auth/callback",
            ));
        }
        Ok(())
    }
}
