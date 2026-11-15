use crate::{
    AuthError, AuthOptions, TokenFuture, TokenProvider,
    discovery::{self, Metadata},
    store::{Record, Store},
    wire::{self, Pair, Reply},
};
use echo_contracts::secret::Secret;
use reqwest::{Client, redirect::Policy};
use serde::Deserialize;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};

#[cfg(test)]
#[path = "auth_tests/mod.rs"]
mod tests;

/// Display only the public user code, verification URI and expiry to the user.
#[derive(Deserialize)]
pub struct LinkInstructions {
    device_code: Secret<String>,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
    #[serde(skip, default = "Instant::now")]
    created: Instant,
}

fn default_interval() -> u64 {
    5
}

struct State {
    record: Record,
    uncertain: bool,
}

pub struct AuthClient {
    options: AuthOptions,
    metadata: Metadata,
    http: Client,
    store: Store,
    state: Mutex<State>,
    generation: AtomicU64,
    epoch: u64,
    started: Instant,
}

impl AuthClient {
    /// Discover endpoints and load/create the key. Never starts linking implicitly.
    ///
    /// # Errors
    /// Returns safe discovery, transport, binding or storage categories.
    pub async fn open(options: AuthOptions) -> Result<Self, AuthError> {
        if options.client_id.is_empty() || options.request_timeout.is_zero() {
            return Err(AuthError::Discovery);
        }
        let http = Client::builder()
            .redirect(Policy::none())
            .timeout(options.request_timeout)
            .build()
            .map_err(|_| AuthError::Transport)?;
        let metadata = discovery::discover(&http, &options).await?;
        let (store, record) = Store::open(&options, &metadata.issuer)?;
        Ok(Self {
            options,
            metadata,
            http,
            store,
            state: Mutex::new(State {
                record,
                uncertain: false,
            }),
            generation: AtomicU64::new(0),
            epoch: wire::unix_now()?,
            started: Instant::now(),
        })
    }

    fn now(&self) -> u64 {
        self.epoch.saturating_add(self.started.elapsed().as_secs())
    }

    /// # Errors
    /// Returns an OAuth or transport failure; never includes server diagnostics.
    pub async fn begin_link(&self) -> Result<LinkInstructions, AuthError> {
        let state = self.state.lock().await;
        let scope = self.options.scopes.join(" ");
        let form = [
            ("client_id", self.options.client_id.as_str()),
            ("scope", &scope),
            ("resource", self.options.resource_url.as_str()),
        ];
        let Reply::Value(mut link) = wire::post::<LinkInstructions>(
            &self.http,
            &self.metadata.device_authorization_endpoint,
            &form,
            Some(&state.record),
            self.now(),
        )
        .await?
        else {
            return Err(AuthError::Protocol);
        };
        discovery::safe_url(&link.verification_uri).map_err(|_| AuthError::Protocol)?;
        if link.device_code.expose().is_empty()
            || link.user_code.is_empty()
            || link.expires_in == 0
            || link.interval == 0
        {
            return Err(AuthError::Protocol);
        }
        link.created = Instant::now();
        Ok(link)
    }

    /// Poll at the advertised interval, adding five seconds after `slow_down`.
    /// Dropping this future cancels polling; it starts no background task.
    /// # Errors
    /// Reports denial, expiry, protocol, storage or transport failure.
    pub async fn await_approval(&self, link: LinkInstructions) -> Result<(), AuthError> {
        let expires = link
            .created
            .checked_add(Duration::from_secs(link.expires_in))
            .ok_or(AuthError::Protocol)?;
        let mut interval = link.interval;
        loop {
            let next = Instant::now()
                .checked_add(Duration::from_secs(interval))
                .ok_or(AuthError::Protocol)?;
            if next >= expires {
                tokio::time::sleep_until(expires).await;
                return Err(AuthError::Expired);
            }
            tokio::time::sleep_until(next).await;
            let mut state = self.state.lock().await;
            let form = [
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", link.device_code.expose().as_str()),
                ("client_id", self.options.client_id.as_str()),
            ];
            let reply = tokio::time::timeout_at(
                expires,
                wire::post::<Pair>(
                    &self.http,
                    &self.metadata.token_endpoint,
                    &form,
                    Some(&state.record),
                    self.now(),
                ),
            )
            .await
            .map_err(|_| AuthError::Expired)??;
            match reply {
                Reply::Pending => {}
                Reply::SlowDown => {
                    interval = interval.checked_add(5).ok_or(AuthError::Protocol)?;
                }
                Reply::Value(pair) => {
                    self.publish(&mut state, Some(pair.grant(self.now())?))?;
                    return Ok(());
                }
            }
        }
    }

    fn publish(&self, state: &mut State, grant: Option<Secret<crate::store::Grant>>) -> Result<(), AuthError> {
        state.uncertain = true;
        let old = std::mem::replace(&mut state.record.grant, grant);
        if let Err(error) = self.store.save(&state.record) {
            state.record.grant = old;
            return Err(error);
        }
        state.uncertain = false;
        self.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    async fn rotate(&self, state: &mut State) -> Result<(), AuthError> {
        if state.uncertain {
            return Err(AuthError::RelinkRequired);
        }
        let grant = state.record.grant.as_ref().ok_or(AuthError::RelinkRequired)?;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", grant.expose().refresh_token.as_str()),
            ("client_id", self.options.client_id.as_str()),
        ];
        // Mark before the request: cancellation/transport failure can mean the
        // server rotated. Never replay an ambiguous refresh in this process.
        state.uncertain = true;
        match wire::post::<Pair>(
            &self.http,
            &self.metadata.token_endpoint,
            &form,
            Some(&state.record),
            self.now(),
        )
        .await
        {
            Ok(Reply::Value(pair)) => self.publish(state, Some(pair.grant(self.now())?)),
            Err(AuthError::RelinkRequired) => {
                self.publish(state, None)?;
                Err(AuthError::RelinkRequired)
            }
            Err(error) => Err(error),
            Ok(_) => Err(AuthError::Protocol),
        }
    }

    /// # Errors
    /// Reports relinking, transport, protocol or storage failure, without retry.
    pub async fn force_refresh(&self) -> Result<Secret<String>, AuthError> {
        let mut state = self.state.lock().await;
        self.rotate(&mut state).await?;
        token(&state)
    }

    /// Revoke remotely before clearing the durable grant. A failed remote revoke
    /// retains the credential so the caller can explicitly retry this operation.
    /// # Errors
    /// Reports transport/storage failures; access remains blocked after failure.
    pub async fn revoke(&self) -> Result<(), AuthError> {
        let mut state = self.state.lock().await;
        let Some(grant) = &state.record.grant else {
            return Ok(());
        };
        let form = [
            ("token", grant.expose().refresh_token.as_str()),
            ("client_id", self.options.client_id.as_str()),
        ];
        let request = self.http.post(&self.metadata.revocation_endpoint).form(&form);
        state.uncertain = true;
        let response = request.send().await.map_err(|_| AuthError::Transport)?;
        if !response.status().is_success() {
            return Err(AuthError::Transport);
        }
        self.publish(&mut state, None)
    }
}

fn token(state: &State) -> Result<Secret<String>, AuthError> {
    if state.uncertain {
        return Err(AuthError::RelinkRequired);
    }
    state
        .record
        .grant
        .as_ref()
        .map(|g| Secret::new(g.expose().access_token.clone()))
        .ok_or(AuthError::RelinkRequired)
}

impl TokenProvider for AuthClient {
    fn access_token(&self) -> TokenFuture<'_> {
        let seen = self.generation.load(Ordering::Acquire);
        Box::pin(async move {
            let mut state = self.state.lock().await;
            let grant = state.record.grant.as_ref().ok_or(AuthError::RelinkRequired)?;
            if seen == self.generation.load(Ordering::Acquire)
                && grant.expose().expires_at_unix <= self.now().saturating_add(300)
            {
                self.rotate(&mut state).await?;
            }
            token(&state)
        })
    }
}
