//! Independent `ChatGPT` device credentials. Claims are decoded without verifying
//! JWT signatures: the configured issuer is trusted over TLS. Callers own code
//! display; this library never launches a browser or discovers a home auth file.
//! Store permissions: Unix 0600; Windows a protected current-user-only DACL,
//! installed on an empty temporary directory before credential bytes are written.
mod auth_clock;
mod auth_device;
mod auth_http;
mod auth_private;
mod auth_store;
mod auth_token;

use echo_config::ChatGptAuthConfig;
use echo_contracts::secret::Secret;
use std::{path::PathBuf, sync::Arc};

/// A credential deliberately without `Debug`, `Display`, `Clone` or serialization.
pub struct ChatGptCredential {
    pub access_token: Secret<String>,
    pub account_id: String,
}

/// Display only in the caller's sign-in UI; never write this prompt to traces.
pub struct DeviceCodePrompt {
    pub user_code: String,
    pub verification_url: String,
}

/// Safe categories. No response body, path or underlying error is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("not signed in")]
    NotSignedIn,
    #[error("invalid authentication configuration")]
    InvalidConfig,
    #[error("invalid credential")]
    InvalidCredential,
    #[error("account claim missing")]
    AccountIdMissing,
    #[error("account mismatch")]
    AccountMismatch,
    #[error("credential already imported")]
    AlreadyImported,
    #[error("authorization denied")]
    AuthorizationDenied,
    #[error("state mismatch")]
    StateMismatch,
    #[error("authentication timed out")]
    Timeout,
    #[error("authentication cancelled")]
    Cancelled,
    #[error("authentication busy")]
    Busy,
    #[error("authentication transport failed")]
    Transport,
    #[error("credential storage failed")]
    Storage,
    #[error("sign-in required")]
    RelinkRequired,
}

pub struct AuthSource {
    config: ChatGptAuthConfig,
    file: PathBuf,
    client: reqwest::Client,
    clock: Arc<dyn auth_clock::Clock>,
    operation: tokio::sync::Mutex<()>,
}

impl AuthSource {
    /// Construct without opening credentials or contacting the issuer.
    ///
    /// # Errors
    /// Rejects unsafe endpoints or unavailable TLS client initialization.
    pub fn new(config: ChatGptAuthConfig, credential_file: PathBuf) -> Result<Self, AuthError> {
        config.validate().map_err(|_| AuthError::InvalidConfig)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| AuthError::Transport)?;
        Ok(Self {
            config,
            file: credential_file,
            client,
            clock: Arc::new(auth_clock::SystemClock::new()),
            operation: tokio::sync::Mutex::new(()),
        })
    }

    /// Return a fresh persisted credential. Due credentials require relinking
    /// until the refresh story supplies the serialized refresh transaction.
    ///
    /// # Errors
    /// Reports missing, invalid, expired or unreadable storage safely.
    pub async fn current(&self) -> Result<ChatGptCredential, AuthError> {
        let _guard = self.operation.lock().await;
        let record = auth_store::load(&self.file)?;
        if record.due(self.clock.unix()?) {
            return Err(AuthError::RelinkRequired);
        }
        Ok(record.credential())
    }

    /// Device sign-in with a callback before the first poll. Dropping this
    /// future cancels its requests and polling; there are no background tasks.
    ///
    /// # Errors
    /// Returns safe denial, timeout, transport, credential or storage categories.
    pub async fn device_sign_in(
        &self,
        callback: impl FnOnce(DeviceCodePrompt),
    ) -> Result<ChatGptCredential, AuthError> {
        let _guard = self.operation.lock().await;
        let record = auth_device::sign_in(self, callback).await?;
        auth_store::save(&self.file, &record)?;
        Ok(record.credential())
    }
}

#[cfg(test)]
#[path = "../tests/auth/mod.rs"]
mod auth_tests;
