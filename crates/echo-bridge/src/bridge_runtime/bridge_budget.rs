//! Host reservations survive cancellation and replacement of backend workers.
use echo_agent_loop::{CredentialError, ModelCredential};
use echo_config::LiveMode;
use echo_contracts::secret::Secret;
use reqwest::header::HeaderMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub(super) struct Budget {
    state: Mutex<(u64, u64)>,
    cap: u64,
}

impl Budget {
    pub fn new(cap: u64) -> Self {
        Self {
            state: Mutex::new((0, 0)),
            cap,
        }
    }

    fn reserve(&self, day: u64) -> Result<(), CredentialError> {
        let mut state = self.state.lock().map_err(|_| CredentialError)?;
        if state.0 != day {
            *state = (day, 0);
        }
        if state.1 >= self.cap {
            return Err(CredentialError);
        }
        state.1 += 1;
        Ok(())
    }
}

pub(super) struct Credentials {
    pub providers: echo_signin::Providers,
    pub budget: Arc<Budget>,
    pub limited: AtomicBool,
    pub authorization_failed: AtomicBool,
}

impl ModelCredential for Credentials {
    async fn headers(&self, mode: LiveMode) -> Result<Secret<HeaderMap>, CredentialError> {
        let headers = self.providers.headers(mode).await.inspect_err(|_| {
            self.authorization_failed.store(true, Ordering::Relaxed);
        })?;
        let day = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 86_400;
        if self.budget.reserve(day).is_err() {
            self.limited.store(true, Ordering::Relaxed);
            return Err(CredentialError);
        }
        // Never refund: publication can race cancellation or an uncertain send.
        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_delegation_reservations_survive_replacement_and_roll_over_once() {
        let retained = Arc::new(Budget::new(2));
        let first = retained.clone();
        assert!(first.reserve(17).is_ok());
        drop(first); // An abandoned runner cannot refund its reservation.
        let replacement = retained.clone();
        assert!(replacement.reserve(17).is_ok());
        assert!(replacement.reserve(17).is_err());
        assert!(replacement.reserve(18).is_ok());
        assert!(replacement.reserve(18).is_ok());
        assert!(replacement.reserve(18).is_err());
        assert!(Budget::new(0).reserve(17).is_err());
    }
}
