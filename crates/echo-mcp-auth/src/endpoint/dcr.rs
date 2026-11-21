use super::AuthClient;
use crate::{
    AuthError,
    endpoint::{EndpointGrant, EndpointRegistration},
};
use std::sync::Arc;

impl AuthClient {
    /// Open a separate endpoint grant using this installation's committed DCR identity.
    ///
    /// # Errors
    /// Returns relink required for an unregistered or mismatched client.
    pub async fn endpoint_grant(&self, registration: Arc<EndpointRegistration>) -> Result<EndpointGrant, AuthError> {
        let state = self.state.lock().await;
        let client_id = state
            .record
            .client_id
            .as_ref()
            .filter(|_| state.record.registration.is_some())
            .ok_or(AuthError::RelinkRequired)?;
        EndpointGrant::open(
            registration,
            self.metadata.clone(),
            self.http.clone(),
            client_id.clone(),
            self.options.scopes.join(" "),
            self.now(),
        )
        .await
    }
}
