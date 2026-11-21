use crate::{
    AuthError, AuthOptions, LinkClient, LinkFailure, TokenFuture, TokenProvider,
    discovery::{self, Metadata},
    proof::{self, PrivateJwk},
    store::{Grant, Record, Registration, Store},
    wire::{self, Pair, Refusal, Reply},
};
use echo_contracts::secret::Secret;
use reqwest::{Client, redirect::Policy};
use serde::Deserialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};

#[path = "endpoint/grant.rs"]
mod endpoint;
pub use endpoint::EndpointGrant;

#[cfg(test)]
#[path = "auth_tests/mod.rs"]
mod tests;

#[derive(Deserialize)]
struct DeviceChallenge {
    device_code: Secret<String>,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

/// Display only the public user code, verification URI and expiry to the user.
pub struct LinkInstructions {
    device_code: Secret<String>,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
    created: Instant,
    identity: LinkIdentity,
    key: Arc<Secret<PrivateJwk>>,
}

struct LinkIdentity {
    client_id: String,
    registration: Option<Registration>,
}

fn default_interval() -> u64 {
    5
}

struct State {
    record: Record,
    uncertain: bool,
    endpoint: Option<endpoint::Access>,
}

/// Public presentation facts from one usable committed grant.
#[derive(Default)]
pub struct GrantStatus {
    pub scopes: Option<Vec<String>>,
    pub uses_legacy_cli: bool,
}

pub struct AuthClient {
    options: AuthOptions<LinkClient>,
    metadata: Metadata,
    http: Client,
    store: Store,
    state: Mutex<State>,
    generation: AtomicU64,
    linked: AtomicBool,
    epoch: u64,
    started: Instant,
    failure: std::sync::Mutex<Option<LinkFailure>>,
}

impl AuthClient {
    /// Discover endpoints and load/create the key. Never starts linking implicitly.
    ///
    /// # Errors
    /// Returns safe discovery, transport, binding or storage categories.
    pub async fn open(options: AuthOptions) -> Result<Self, AuthError> {
        Self::open_with_ca(options, None, None).await
    }

    /// Use an independently chosen UTC epoch for guarded simulated runs.
    /// Only the initial wall sample changes; elapsed Tokio time still drives expiry.
    /// # Errors
    /// Returns the same categories as `open`.
    pub async fn open_at_epoch(options: AuthOptions, epoch_unix: u64) -> Result<Self, AuthError> {
        Self::open_with_ca(options, Some(epoch_unix), None).await
    }

    /// Add explicit PEM trust roots without changing native trust or hostname checks.
    /// # Errors
    /// Returns safe transport errors for malformed trust, or the categories of `open`.
    pub async fn open_with_ca(
        options: AuthOptions,
        epoch: Option<u64>,
        ca_pem: Option<&[u8]>,
    ) -> Result<Self, AuthError> {
        Self::open_configured(options.explicit(), epoch, ca_pem).await
    }

    /// Discover a configured client or prepare to register on explicit link.
    /// # Errors
    /// Returns safe discovery, transport, binding or storage categories.
    pub async fn open_configured(
        options: AuthOptions<LinkClient>,
        epoch: Option<u64>,
        ca_pem: Option<&[u8]>,
    ) -> Result<Self, AuthError> {
        options.validate()?;
        let mut builder = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(options.request_timeout);
        if let Some(pem) = ca_pem {
            let roots = reqwest::Certificate::from_pem_bundle(pem).map_err(|_| AuthError::Transport)?;
            if roots.is_empty() {
                return Err(AuthError::Transport);
            }
            for root in roots {
                builder = builder.add_root_certificate(root);
            }
        }
        let http = builder.build().map_err(|_| AuthError::Transport)?;
        let metadata = discovery::discover(&http, &options.resource_url).await?;
        let (store, record) = Store::open(&options, &metadata.issuer)?;
        Ok(Self {
            options,
            metadata,
            http,
            store,
            linked: AtomicBool::new(record.grant.is_some()),
            state: Mutex::new(State {
                record,
                uncertain: false,
                endpoint: None,
            }),
            generation: AtomicU64::new(0),
            epoch: match epoch {
                Some(epoch) => epoch,
                None => wire::unix_now()?,
            },
            started: Instant::now(),
            failure: std::sync::Mutex::default(),
        })
    }

    /// Take the sanitized facts of the link step that failed last, if any:
    /// its name, the HTTP status and the OAuth error code. Nothing else.
    pub fn take_link_failure(&self) -> Option<LinkFailure> {
        self.failure.lock().ok().and_then(|mut last| last.take())
    }

    fn note(&self, failure: Option<LinkFailure>) {
        if let Ok(mut last) = self.failure.lock() {
            *last = failure;
        }
    }

    fn refused(&self, step: &'static str, refusal: Refusal) -> AuthError {
        let (failure, error) = refusal.at(step);
        self.note(Some(failure));
        error
    }

    /// Passive local grant status for presentation. Never acquires a token,
    /// refreshes, reads a file or probes the authorization server. False while
    /// a rotation/revocation is in flight or its outcome is uncertain.
    #[must_use]
    pub fn has_grant(&self) -> bool {
        self.linked.load(Ordering::Acquire)
    }

    /// Copy only the scopes of the locally committed usable grant.
    pub async fn granted_scopes(&self) -> Option<Vec<String>> {
        self.grant_status().await.scopes
    }

    /// Read presentation facts without acquiring a token or changing the grant.
    pub async fn grant_status(&self) -> GrantStatus {
        let state = self.state.lock().await;
        let Some(grant) = state.record.grant.as_ref().filter(|_| self.has_grant()) else {
            return GrantStatus::default();
        };
        GrantStatus {
            scopes: Some(
                grant
                    .expose()
                    .scope
                    .split_ascii_whitespace()
                    .map(str::to_owned)
                    .collect(),
            ),
            uses_legacy_cli: state.record.client_id.as_deref()
                == Some("port1/systems/zero/oauth-clients/command-line-interface"),
        }
    }

    fn now(&self) -> u64 {
        self.epoch.saturating_add(self.started.elapsed().as_secs())
    }

    /// Port OS accepts one device authorization per `DPoP` key, so every link
    /// signs with a new key. The committed grant keeps its own key until an
    /// approval replaces the grant and the key together.
    /// # Errors
    /// Returns an OAuth or transport failure; never includes server diagnostics.
    pub async fn begin_link(&self) -> Result<LinkInstructions, AuthError> {
        let state = self.state.lock().await;
        self.note(None);
        let key = Arc::new(proof::generate());
        let scope = self.options.scopes.join(" ");
        let identity = self.link_identity(&state.record, &scope).await?;
        let form = [
            ("client_id", identity.client_id.as_str()),
            ("scope", &scope),
            ("resource", self.options.resource_url.as_str()),
        ];
        let Reply::Value(link) = wire::post::<DeviceChallenge>(
            &self.http,
            &self.metadata.device_authorization_endpoint,
            &form,
            Some(key.expose()),
            self.now(),
        )
        .await
        .map_err(|refusal| self.refused("device_authorization", refusal))?
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
        Ok(LinkInstructions {
            device_code: link.device_code,
            user_code: link.user_code,
            verification_uri: link.verification_uri,
            expires_in: link.expires_in,
            interval: link.interval,
            created: Instant::now(),
            identity,
            key,
        })
    }

    async fn link_identity(&self, record: &Record, scope: &str) -> Result<LinkIdentity, AuthError> {
        let LinkClient::Automatic { name } = &self.options.client_id else {
            let client_id = record.client_id.clone().ok_or(AuthError::Storage)?;
            return Ok(LinkIdentity {
                client_id,
                registration: record.registration.clone(),
            });
        };
        let registration = Registration {
            name: name.clone(),
            scope: scope.into(),
        };
        let client_id = if record.registration.as_ref() == Some(&registration) {
            record.client_id.clone().ok_or(AuthError::Storage)?
        } else {
            wire::register(&self.http, self.metadata.registration_url()?, name, scope)
                .await
                .map_err(|refusal| self.refused("registration", refusal))?
        };
        Ok(LinkIdentity {
            client_id,
            registration: Some(registration),
        })
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
                ("grant_type", wire::DEVICE_GRANT),
                ("device_code", link.device_code.expose().as_str()),
                ("client_id", link.identity.client_id.as_str()),
            ];
            let reply = tokio::time::timeout_at(
                expires,
                wire::post::<Pair>(
                    &self.http,
                    &self.metadata.token_endpoint,
                    &form,
                    Some(link.key.expose()),
                    self.now(),
                ),
            )
            .await
            .map_err(|_| AuthError::Expired)?
            .map_err(|refusal| self.refused("token", refusal))?;
            match reply {
                Reply::Pending => {}
                Reply::SlowDown => {
                    interval = interval.checked_add(5).ok_or(AuthError::Protocol)?;
                }
                Reply::Value(pair) => {
                    let linked = Some((link.identity, link.key));
                    self.publish(&mut state, Some(pair.grant(self.now())?), linked)?;
                    return Ok(());
                }
            }
        }
    }

    fn publish(
        &self,
        state: &mut State,
        grant: Option<Secret<Grant>>,
        linked: Option<(LinkIdentity, Arc<Secret<PrivateJwk>>)>,
    ) -> Result<(), AuthError> {
        self.linked.store(false, Ordering::Release);
        state.uncertain = true;
        let current = &state.record;
        let reset_endpoint = linked.is_some() || grant.is_none();
        let (client_id, registration, key) = match linked {
            Some((identity, key)) => (Some(identity.client_id), identity.registration, key),
            None => (
                current.client_id.clone(),
                current.registration.clone(),
                current.private_key_jwk.clone(),
            ),
        };
        let record = Record {
            version: current.version,
            issuer: current.issuer.clone(),
            resource: current.resource.clone(),
            client_id,
            registration,
            private_key_jwk: key,
            grant,
        };
        self.store.save(&record)?;
        state.record = record;
        if reset_endpoint {
            state.endpoint = None;
        }
        state.uncertain = false;
        self.linked.store(state.record.grant.is_some(), Ordering::Release);
        self.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    async fn rotate(&self, state: &mut State) -> Result<(), AuthError> {
        let resource = state.record.resource.clone();
        let grant = self.redeem_refresh(state, &resource).await?;
        if state.endpoint.as_ref().is_some_and(|access| access.matches(&grant)) {
            return Err(AuthError::Protocol);
        }
        self.publish(state, Some(grant), None)
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
            (
                "client_id",
                state.record.client_id.as_deref().ok_or(AuthError::Storage)?,
            ),
        ];
        let request = self.http.post(&self.metadata.revocation_endpoint).form(&form);
        self.linked.store(false, Ordering::Release);
        state.uncertain = true;
        let response = request.send().await.map_err(|_| AuthError::Transport)?;
        if !response.status().is_success() {
            return Err(AuthError::Transport);
        }
        self.publish(&mut state, None, None)
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
