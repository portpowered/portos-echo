use super::{EndpointRegistration, EndpointTokenSource};
use crate::{
    AuthError, TokenFuture,
    auth::DeviceChallenge,
    discovery::Metadata,
    proof::{self, PrivateJwk},
    store::Grant,
    valid_text,
    wire::{self, Pair, Reply},
};
use echo_contracts::secret::Secret;
use reqwest::Client;
use serde::{Deserialize, Serialize, Serializer};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::time::{Instant, sleep_until, timeout_at};

/// Public device-code instructions and the private, single-use poll context.
pub struct EndpointApproval {
    challenge: DeviceChallenge,
    key: Secret<PrivateJwk>,
    expires: Instant,
    generation: u64,
}

impl EndpointApproval {
    #[must_use]
    pub fn user_code(&self) -> &str {
        &self.challenge.user_code
    }

    #[must_use]
    pub fn verification_uri(&self) -> &str {
        &self.challenge.verification_uri
    }

    #[must_use]
    pub fn expires_in(&self) -> u64 {
        self.challenge.expires_in
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct SavedGrant {
    issuer: String,
    client_id: String,
    #[serde(serialize_with = "serialize_secret")]
    private_key_jwk: Secret<PrivateJwk>,
    #[serde(serialize_with = "serialize_secret")]
    access_token: Secret<String>,
    #[serde(serialize_with = "serialize_secret")]
    refresh_token: Secret<String>,
    expires_at_unix: u64,
    scope: String,
}

fn serialize_secret<S: Serializer, T: Serialize>(value: &Secret<T>, serializer: S) -> Result<S::Ok, S::Error> {
    value.expose().serialize(serializer)
}

/// An unbound device-code grant, using the registered client's issuer and its own key.
pub struct EndpointGrant {
    registration: Arc<EndpointRegistration>,
    metadata: Metadata,
    http: Client,
    client_id: String,
    scope: String,
    epoch: u64,
    started: Instant,
}

impl EndpointGrant {
    pub(crate) async fn open(
        registration: Arc<EndpointRegistration>,
        metadata: Metadata,
        http: Client,
        client_id: String,
        scope: String,
        epoch: u64,
    ) -> Result<Self, AuthError> {
        let grant = Self {
            registration,
            metadata,
            http,
            client_id,
            scope,
            epoch,
            started: Instant::now(),
        };
        if let Some(saved) = &grant.registration.record.lock().await.endpoint_grant {
            if saved.issuer != grant.metadata.issuer || saved.client_id != grant.client_id {
                return Err(AuthError::RelinkRequired);
            }
            proof::signing_key(saved.private_key_jwk.expose())?;
            if !valid_text(saved.access_token.expose()) || !valid_text(saved.refresh_token.expose()) {
                return Err(AuthError::Storage);
            }
        }
        Ok(grant)
    }

    fn now(&self) -> u64 {
        self.epoch.saturating_add(self.started.elapsed().as_secs())
    }

    /// True when a committed endpoint grant exists.
    pub async fn has_grant(&self) -> bool {
        self.registration.record.lock().await.endpoint_grant.is_some()
    }

    /// Request approval without a protected-resource audience.
    ///
    /// # Errors
    /// Returns safe approval, protocol or transport categories.
    pub async fn begin_approval(&self) -> Result<EndpointApproval, AuthError> {
        let generation = self.registration.generation.load(Ordering::Acquire);
        let key = proof::generate();
        let form = [("client_id", self.client_id.as_str()), ("scope", &self.scope)];
        let Reply::Value(challenge) = wire::post::<DeviceChallenge>(
            &self.http,
            &self.metadata.device_authorization_endpoint,
            &form,
            Some(key.expose()),
            self.now(),
        )
        .await
        .map_err(AuthError::from)?
        else {
            return Err(AuthError::Protocol);
        };
        challenge.validate()?;
        let expires = Instant::now()
            .checked_add(Duration::from_secs(challenge.expires_in))
            .ok_or(AuthError::Protocol)?;
        Ok(EndpointApproval {
            challenge,
            key,
            expires,
            generation,
        })
    }

    /// Poll until approved and publish only after durable storage.
    ///
    /// # Errors
    /// Reports denial, expiry, invalidated approval, protocol or storage failure.
    pub async fn await_approval(&self, approval: EndpointApproval) -> Result<(), AuthError> {
        let mut interval = approval.challenge.interval;
        let form = [
            ("grant_type", wire::DEVICE_GRANT),
            ("device_code", approval.challenge.device_code.expose().as_str()),
            ("client_id", self.client_id.as_str()),
        ];
        loop {
            let next = Instant::now()
                .checked_add(Duration::from_secs(interval))
                .ok_or(AuthError::Protocol)?;
            if next >= approval.expires {
                sleep_until(approval.expires).await;
                return Err(AuthError::Expired);
            }
            sleep_until(next).await;
            let mut record = self.registration.record.lock().await;
            if approval.generation != self.registration.generation.load(Ordering::Acquire) {
                return Err(AuthError::RelinkRequired);
            }
            let reply = timeout_at(approval.expires, self.redeem(&form, &approval.key))
                .await
                .map_err(|_| AuthError::Expired)??;
            match reply {
                Reply::Pending => {}
                Reply::SlowDown => interval = interval.checked_add(5).ok_or(AuthError::Protocol)?,
                Reply::Value(pair) => {
                    let saved = self.saved_grant(pair, approval.key)?;
                    return self.publish(&mut record, saved);
                }
            }
        }
    }

    /// Refresh once after resolving the installation to obtain its new access claims.
    ///
    /// # Errors
    /// Reports safe relink, protocol, transport or storage categories.
    pub async fn refresh_installation(&self) -> Result<(), AuthError> {
        let mut record = self.registration.record.lock().await;
        self.rotate(&mut record).await
    }

    async fn redeem(&self, form: &[(&str, &str)], key: &Secret<PrivateJwk>) -> Result<Reply<Pair>, AuthError> {
        wire::post(
            &self.http,
            &self.metadata.token_endpoint,
            form,
            Some(key.expose()),
            self.now(),
        )
        .await
        .map_err(AuthError::from)
    }

    fn saved_grant(&self, pair: Pair, key: Secret<PrivateJwk>) -> Result<SavedGrant, AuthError> {
        SavedGrant::from_pair(self, pair.grant(self.now())?.expose(), key)
    }

    fn publish(&self, record: &mut super::registration::Record, saved: SavedGrant) -> Result<(), AuthError> {
        record.endpoint_grant = Some(saved);
        if let Err(error) = self.registration.save(record) {
            record.endpoint_grant = None;
            return Err(error);
        }
        self.registration.rotations.fetch_add(1, Ordering::Release);
        Ok(())
    }

    async fn rotate(&self, record: &mut super::registration::Record) -> Result<(), AuthError> {
        let saved = record.endpoint_grant.take().ok_or(AuthError::RelinkRequired)?;
        // Persist the removal before sending a token that the issuer may consume.
        // A lost response, task cancellation or restart then requires fresh approval.
        self.registration.save(record)?;
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", saved.refresh_token.expose().as_str()),
            ("client_id", self.client_id.as_str()),
        ];
        let Reply::Value(pair) = self.redeem(&form, &saved.private_key_jwk).await? else {
            return Err(AuthError::Protocol);
        };
        self.publish(record, self.saved_grant(pair, saved.private_key_jwk)?)
    }
}

impl SavedGrant {
    fn from_pair(client: &EndpointGrant, grant: &Grant, key: Secret<PrivateJwk>) -> Result<Self, AuthError> {
        if !valid_text(&grant.access_token) || !valid_text(&grant.refresh_token) {
            return Err(AuthError::Protocol);
        }
        Ok(Self {
            issuer: client.metadata.issuer.clone(),
            client_id: client.client_id.clone(),
            private_key_jwk: key,
            access_token: Secret::new(grant.access_token.clone()),
            refresh_token: Secret::new(grant.refresh_token.clone()),
            expires_at_unix: grant.expires_at_unix,
            scope: grant.scope.clone(),
        })
    }
}

impl EndpointTokenSource for EndpointGrant {
    fn access_token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            let observed = self.registration.rotations.load(Ordering::Acquire);
            let mut record = self.registration.record.lock().await;
            let saved = record.endpoint_grant.as_ref().ok_or(AuthError::RelinkRequired)?;
            let expiring = saved.expires_at_unix <= self.now().saturating_add(300);
            if expiring && observed == self.registration.rotations.load(Ordering::Acquire) {
                self.rotate(&mut record).await?;
            }
            let saved = record.endpoint_grant.as_ref().ok_or(AuthError::RelinkRequired)?;
            Ok(Secret::new(saved.access_token.expose().clone()))
        })
    }
}
