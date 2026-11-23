//! Credential acquisition is auth-owned; publication and cancellation are shared.
use crate::{Providers, SigninError};
use echo_agent_loop::{CredentialError, ModelCredential};
use echo_config::LiveMode;
use echo_contracts::secret::Secret;
use echo_live::call::{CallError, CredentialProvider, LiveCredential};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};

impl Providers {
    async fn guarded<T>(&self, operation: impl Future<Output = Result<T, SigninError>>) -> Result<T, SigninError> {
        let mut cancelled = self.state.cancellation.subscribe();
        if *self.state.kill.borrow() {
            return Err(SigninError::CredentialFile);
        }
        tokio::select! {
            biased;
            _ = cancelled.changed() => Err(SigninError::CredentialFile),
            result = operation => {
                // A completed on/off pulse still invalidates the old acquisition.
                if cancelled.has_changed().unwrap_or(true) || *self.state.kill.borrow() {
                    return Err(SigninError::CredentialFile);
                }
                result
            }
        }
    }

    async fn acquire(&self) -> Result<LiveCredential, SigninError> {
        self.guarded(async {
            if let Some(auth) = &self.state.auth {
                let value = auth.current().await?;
                Ok(LiveCredential::Chatgpt {
                    token: value.access_token,
                    account_id: value.account_id,
                })
            } else {
                let bytes = tokio::fs::read(&self.state.credential_file)
                    .await
                    .map_err(|_| SigninError::CredentialFile)?;
                let text = String::from_utf8(bytes).map_err(|_| SigninError::CredentialFile)?;
                Ok(LiveCredential::ApiKey { key: key(text)? })
            }
        })
        .await
    }
}

impl CredentialProvider for Providers {
    async fn credential(&self) -> Result<LiveCredential, CallError> {
        self.acquire().await.map_err(|_| CallError::Authorization)
    }
}

impl ModelCredential for Providers {
    async fn headers(&self, mode: LiveMode) -> Result<Secret<HeaderMap>, CredentialError> {
        if mode != self.mode() {
            return Err(CredentialError);
        }
        let credential = self.acquire().await.map_err(|_| CredentialError)?;
        headers(credential).map_err(|_| CredentialError)
    }
}

/// The one API-key rule for the credential file and every writer of it:
/// drop one trailing LF or CRLF, then require a non-blank header value.
///
/// # Errors
/// `CredentialFile` for empty, control or non-header text; never the text.
pub fn api_key(text: String) -> Result<Secret<String>, SigninError> {
    key(text)
}

fn key(mut text: String) -> Result<Secret<String>, SigninError> {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    if text.trim().is_empty() || text.chars().any(char::is_control) {
        return Err(SigninError::CredentialFile);
    }
    HeaderValue::from_str(&text).map_err(|_| SigninError::CredentialFile)?;
    Ok(Secret::new(text))
}

fn headers(credential: LiveCredential) -> Result<Secret<HeaderMap>, SigninError> {
    let mut headers = HeaderMap::new();
    let token = match credential {
        LiveCredential::Chatgpt { token, account_id } => {
            let mut account = HeaderValue::from_str(&account_id).map_err(|_| SigninError::CredentialFile)?;
            account.set_sensitive(true);
            headers.insert("chatgpt-account-id", account);
            token
        }
        LiveCredential::ApiKey { key } => key,
    };
    let mut bearer =
        HeaderValue::from_str(&format!("Bearer {}", token.expose())).map_err(|_| SigninError::CredentialFile)?;
    bearer.set_sensitive(true);
    headers.insert(AUTHORIZATION, bearer);
    Ok(Secret::new(headers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OperatorCommand, resolve_mode};
    use echo_config::{ChatGptAuthConfig, LiveConfig};

    fn providers() -> Providers {
        resolve_mode(
            LiveConfig {
                mode: LiveMode::ApiKey,
                credential_file: "unused-synthetic-path".into(),
                voice: None,
                session_idle_seconds: 60,
            },
            ChatGptAuthConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn key_and_headers_preserve_bytes_and_hide_diagnostics() {
        for value in ["synthetic-key", "synthetic-key\n", "synthetic-key\r\n"] {
            let secret = key(value.into()).unwrap();
            assert_eq!(secret.expose(), "synthetic-key");
            let map = headers(LiveCredential::ApiKey { key: secret }).unwrap();
            assert_eq!(map.expose()[AUTHORIZATION], "Bearer synthetic-key");
            assert!(map.expose()[AUTHORIZATION].is_sensitive());
            assert!(!map.expose().contains_key("chatgpt-account-id"));
        }
        for value in ["", " ", "\n", "key\n\n", "key\r", "key\0", "key\t"] {
            assert!(matches!(key(value.into()), Err(SigninError::CredentialFile)));
        }
        let map = headers(LiveCredential::Chatgpt {
            token: Secret::new("synthetic-token".into()),
            account_id: "synthetic-account".into(),
        })
        .unwrap();
        assert_eq!(map.expose()["chatgpt-account-id"], "synthetic-account");
        assert!(map.expose()["chatgpt-account-id"].is_sensitive());
        assert!(matches!(
            headers(LiveCredential::Chatgpt {
                token: Secret::new("synthetic".into()),
                account_id: "bad\naccount".into(),
            }),
            Err(SigninError::CredentialFile)
        ));
    }

    #[tokio::test]
    async fn kill_cancels_pending_and_ready_publication_even_after_clear() {
        let providers = providers();
        let clone = providers.clone();
        let mut signal = providers.subscribe_kill();
        let operation = providers.guarded(async {
            clone
                .execute(OperatorCommand::KillSwitch { enabled: true }, &mut Vec::new())
                .await
                .unwrap();
            clone
                .execute(OperatorCommand::KillSwitch { enabled: false }, &mut Vec::new())
                .await
                .unwrap();
            Ok(42)
        });
        assert_eq!(operation.await, Err(SigninError::CredentialFile));
        signal.changed().await.unwrap();
        assert!(!*signal.borrow());
        let pending = providers.guarded(std::future::pending::<Result<u8, SigninError>>());
        tokio::pin!(pending);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(pending.as_mut().poll(cx).is_pending())).await);
        clone
            .execute(OperatorCommand::KillSwitch { enabled: true }, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(pending.await, Err(SigninError::CredentialFile));
        assert_eq!(
            providers.guarded(async { Ok(1) }).await,
            Err(SigninError::CredentialFile)
        );
        clone
            .execute(OperatorCommand::KillSwitch { enabled: false }, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(providers.guarded(async { Ok(7) }).await, Ok(7));
    }
}
