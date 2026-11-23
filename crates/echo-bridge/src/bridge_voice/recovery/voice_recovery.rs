//! After readiness nothing is fatal: a failed generation is reported, waited
//! out with bounded backoff and replaced. Only a requested stop, the kill
//! switch, or unusable configuration before first readiness end it.
use super::retained_link::Retained;
use crate::Error;
use echo_link::{LinkConnector, LinkEvent, RunFailure};
use std::{future::Future, pin::Pin, time::Duration};
use tokio::time::Instant;

const FIRST_DELAY: Duration = Duration::from_secs(1);
pub(super) const DELAY_CAP: Duration = Duration::from_secs(60);

pub(super) async fn stopped(mut stopping: tokio::sync::watch::Receiver<bool>) {
    loop {
        if *stopping.borrow_and_update() || stopping.changed().await.is_err() {
            return;
        }
    }
}

/// 1 s doubling to the 60 s cap per consecutive failed generation.
#[derive(Default)]
pub(super) struct Backoff {
    failures: u32,
}

impl Backoff {
    pub fn reset(&mut self) {
        self.failures = 0;
    }

    /// The attempt number of the retry and the wait that precedes it.
    pub fn next(&mut self) -> (u32, Duration) {
        let factor = 1_u32.checked_shl(self.failures).unwrap_or(u32::MAX);
        self.failures = self.failures.saturating_add(1);
        (self.failures, FIRST_DELAY.saturating_mul(factor).min(DELAY_CAP))
    }
}

/// What one authenticated generation (process, account, device, service) did.
pub(super) struct Round {
    pub result: Result<(), Error>,
    /// When this generation first reported readiness.
    pub ready: Option<Instant>,
    /// A call connected or a run completed, which ends a failure streak.
    pub served: bool,
    /// The operator's kill switch ended it, which is as final as a stop.
    pub killed: bool,
}

/// The owner of generations and of what the operator sees between them.
pub(super) trait Stage<C: LinkConnector> {
    fn generation(
        &mut self,
        link: &Retained<C>,
        cancelled: Pin<&mut impl Future<Output = ()>>,
    ) -> impl Future<Output = Round>;
    /// The operator asked for the stop that `cancelled` reported.
    fn requested(&self) -> bool;
    /// Trace, health and screen for a failure that will be retried.
    fn recovering(&mut self, error: &Error, attempt: u32, delay: Duration, linked: bool) -> impl Future<Output = ()>;
    /// A device event seen while no generation is running.
    fn device(&mut self, event: LinkEvent<()>);
}

/// Exit is reserved for a stop (requested, or the kill switch) and for
/// configuration or credentials that are unusable before first readiness.
/// `control` alone is not a stop: a muted or cut run reports it too.
pub(super) fn terminal(error: &Error, ever_ready: bool, stopped: bool) -> bool {
    use echo_mcp_auth::AuthError;
    stopped
        || (!ever_ready
            && matches!(
                error,
                Error::Input
                    | Error::CodexHomeRequired
                    | Error::CodexHome
                    | Error::CodexBinary
                    | Error::CodexCaFile
                    | Error::CodexImport
                    | Error::Authorization
                    | Error::Mcp(AuthError::RelinkRequired | AuthError::AccessDenied | AuthError::Expired)
            ))
}

/// Run generations until one ends terminally. The device link is kept across
/// them, so a wake during a retry delay starts the next generation at once.
/// A stop requested during a delay is a clean stop.
pub(super) async fn supervise<C: LinkConnector>(
    stage: &mut impl Stage<C>,
    link: &Retained<C>,
    cancelled: impl Future<Output = ()>,
) -> Result<(), Error> {
    tokio::pin!(cancelled);
    let mut backoff = Backoff::default();
    let mut ever_ready = false;
    let result = loop {
        let round = stage.generation(link, cancelled.as_mut()).await;
        ever_ready |= round.ready.is_some();
        let stopped = stage.requested() || round.killed;
        let error = match round.result {
            Err(Error::Authorization) if crate::bridge_settings::openai_account::owns_relink() => {
                break Err(Error::Authorization);
            }
            Err(error) if !terminal(&error, ever_ready, stopped) => error,
            result => break result,
        };
        if round.served || round.ready.is_some_and(|at| at.elapsed() >= DELAY_CAP) {
            backoff.reset();
        }
        let code = if error == Error::Authorization {
            RunFailure::RelinkRequired
        } else {
            RunFailure::Unavailable
        };
        let (attempt, delay) = backoff.next();
        // A device-side failure straight after another failure means the kept
        // connection itself is suspect: the next generation dials a fresh one.
        let linked = link.settle(code, attempt == 1 || error != Error::Wyoming).await;
        stage.recovering(&error, attempt, delay, linked).await;
        let stopped = tokio::select! {
            biased;
            () = cancelled.as_mut() => true,
            () = crate::bridge_settings::openai_account::requested() => true,
            () = link.rest(delay, |event| stage.device(event)) => false,
        };
        if stopped {
            break Ok(());
        }
    };
    link.close().await;
    result
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
