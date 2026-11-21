use super::{AuthClient, State};
use crate::{
    AuthError, TokenFuture, discovery,
    endpoint::EndpointTokenSource,
    store::Grant,
    valid_text,
    wire::{self, Pair, Reply},
};
use echo_contracts::{endpoint::ENDPOINT_SCOPES, secret::Secret};
use std::sync::atomic::Ordering;

pub(super) struct Access {
    resource: String,
    token: Secret<String>,
    expires: u64,
    claimed: bool,
}

impl Access {
    pub(super) fn matches(&self, grant: &Secret<Grant>) -> bool {
        self.token.expose() == &grant.expose().access_token
    }
}

/// An audience view of the existing registered Port OS grant.
pub struct EndpointGrant<'a> {
    owner: &'a AuthClient,
    resource: String,
}

impl AuthClient {
    /// Use the configured endpoint API origin as its resource indicator.
    ///
    /// # Errors
    /// Refuses invalid destinations and the MCP resource's own origin audience.
    pub fn endpoint_grant(&self, api_base: &str) -> Result<EndpointGrant<'_>, AuthError> {
        let resource = discovery::safe_url(api_base)?.origin().ascii_serialization();
        if self.options.resource_url.trim_end_matches('/') == resource {
            return Err(AuthError::Protocol);
        }
        Ok(EndpointGrant { owner: self, resource })
    }

    /// Whether the committed grant can serve the endpoint at `api_base`: one
    /// DCR sign-in approved that REST audience with the endpoint scopes. A grant
    /// from before the two-audience sign-in is never refreshed for it; it needs a relink.
    pub async fn endpoint_ready(&self, api_base: &str) -> bool {
        let Ok(endpoint) = self.endpoint_grant(api_base) else {
            return false;
        };
        let state = self.state.lock().await;
        self.has_grant() && endpoint.require_registered(&state).is_ok()
    }

    pub(super) async fn redeem_refresh(&self, state: &mut State, resource: &str) -> Result<Secret<Grant>, AuthError> {
        if state.uncertain || state.record.grant.is_none() {
            return Err(AuthError::RelinkRequired);
        }
        self.linked.store(false, Ordering::Release);
        state.uncertain = true;
        if state.record.registration.is_some() {
            // The shared DCR refresh may be spent even if this process stops
            // before receiving the response. Neither audience can replay it.
            let grant = state.record.grant.take();
            let saved = self.store.save(&state.record);
            state.record.grant = grant;
            saved?;
        }
        let grant = state.record.grant.as_ref().ok_or(AuthError::RelinkRequired)?;
        let resources = grant.expose().resources.clone();
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", grant.expose().refresh_token.as_str()),
            (
                "client_id",
                state.record.client_id.as_deref().ok_or(AuthError::Storage)?,
            ),
            ("resource", resource),
        ];
        let result = wire::post::<Pair>(
            &self.http,
            &self.metadata.token_endpoint,
            &form,
            Some(state.record.private_key_jwk.expose()),
            self.now(),
        )
        .await
        .map_err(AuthError::from);
        match result {
            Ok(Reply::Value(pair)) => pair.grant(self.now(), &resources),
            Err(AuthError::RelinkRequired) => {
                self.publish(state, None, None)?;
                Err(AuthError::RelinkRequired)
            }
            Err(error) => Err(error),
            Ok(_) => Err(AuthError::Protocol),
        }
    }
}

impl EndpointGrant<'_> {
    /// Refresh both audience tokens once the installation can supply its claim.
    ///
    /// # Errors
    /// Reports relinking, protocol, transport or storage failure.
    pub async fn refresh_installation(&self) -> Result<(), AuthError> {
        let mut state = self.owner.state.lock().await;
        self.require_registered(&state)?;
        if self.cached(&state).is_some_and(|access| access.claimed) {
            return Ok(());
        }
        self.owner.rotate(&mut state).await?;
        self.rotate(&mut state, true).await
    }

    fn require_registered(&self, state: &State) -> Result<(), AuthError> {
        if state.uncertain || state.record.registration.is_none() {
            return Err(AuthError::RelinkRequired);
        }
        let grant = state.record.grant.as_ref().ok_or(AuthError::RelinkRequired)?.expose();
        let scopes: Vec<_> = grant.scope.split_ascii_whitespace().collect();
        if !grant.resources.contains(&self.resource) || ENDPOINT_SCOPES.iter().any(|scope| !scopes.contains(scope)) {
            return Err(AuthError::RelinkRequired);
        }
        Ok(())
    }

    fn cached<'a>(&self, state: &'a State) -> Option<&'a Access> {
        state
            .endpoint
            .as_ref()
            .filter(|access| access.resource == self.resource)
    }

    async fn rotate(&self, state: &mut State, claimed: bool) -> Result<(), AuthError> {
        let minted = self.owner.redeem_refresh(state, &self.resource).await?;
        let minted = minted.expose();
        let mcp = state.record.grant.as_ref().ok_or(AuthError::RelinkRequired)?.expose();
        if !valid_text(&minted.access_token)
            || !valid_text(&minted.refresh_token)
            || minted.access_token == mcp.access_token
        {
            return Err(AuthError::Protocol);
        }
        state.record.grant = Some(Secret::new(Grant {
            access_token: mcp.access_token.clone(),
            expires_at_unix: mcp.expires_at_unix,
            scope: mcp.scope.clone(),
            refresh_token: minted.refresh_token.clone(),
            resources: mcp.resources.clone(),
        }));
        self.owner.store.save(&state.record)?;
        state.endpoint = Some(Access {
            resource: self.resource.clone(),
            token: Secret::new(minted.access_token.clone()),
            expires: minted.expires_at_unix,
            claimed,
        });
        state.uncertain = false;
        self.owner.linked.store(true, Ordering::Release);
        Ok(())
    }
}

impl EndpointTokenSource for EndpointGrant<'_> {
    fn access_token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            let mut state = self.owner.state.lock().await;
            self.require_registered(&state)?;
            let cached = self.cached(&state);
            if cached.is_none_or(|access| access.expires <= self.owner.now().saturating_add(300)) {
                let claimed = cached.is_some_and(|access| access.claimed);
                self.rotate(&mut state, claimed).await?;
            }
            let access = self.cached(&state).ok_or(AuthError::RelinkRequired)?;
            Ok(Secret::new(access.token.expose().clone()))
        })
    }

    fn fresh_token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            let mut state = self.owner.state.lock().await;
            self.require_registered(&state)?;
            let claimed = self.cached(&state).is_some_and(|access| access.claimed);
            self.rotate(&mut state, claimed).await?;
            let access = self.cached(&state).ok_or(AuthError::RelinkRequired)?;
            Ok(Secret::new(access.token.expose().clone()))
        })
    }
}
