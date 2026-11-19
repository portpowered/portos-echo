//! Runtime-owned dispatch: no detached transactions survive the voice owner.
use super::{SettingsError, SettingsHost, settings_restart::Restart};
use echo_config::EffectiveConfig;
use echo_screen_contract::settings_v1::{LinkStatusV1, PromptTextsV1};
use echo_screen_server::{SettingsOperation, SettingsReply, SettingsRequest, settings_channel};
use std::future::Future;
use tokio::sync::mpsc;

/// One serialized host and one restart request per service generation.
/// The caller must still await resource cleanup and arbitrate shutdown/kill
/// before attempting exec. Completion here is never proof of replacement.
pub struct SettingsService {
    host: Box<SettingsHost>,
    inbox: mpsc::Receiver<SettingsRequest>,
    restart: Restart,
}

/// Accepted state survives resource cleanup without reopening the home. A
/// failed exec may remount it alongside the unchanged active configuration.
pub struct SettingsCompletion {
    pub restart_requested: bool,
    pub host: Box<SettingsHost>,
}

impl SettingsService {
    /// Retain the original base and accepted overlay from a single startup read.
    /// Prompt texts must be the already admitted runtime cache, not a second read.
    ///
    /// # Errors
    /// Refuses unavailable home, invalid config or inadmissible prompt texts.
    pub fn open(
        config: EffectiveConfig,
        prompts: PromptTextsV1,
    ) -> Result<(Self, mpsc::Sender<SettingsRequest>), SettingsError> {
        let warnings = config.warning.into_iter().map(|error| error.to_string()).collect();
        let host = SettingsHost::open(config.base, config.overlay, prompts, warnings)?;
        Ok(Self::resume(Box::new(host)))
    }

    /// Remount accepted state after a failed replacement. The new signal starts
    /// clear: merely restoring the surface must never create an exec retry loop.
    /// An explicit settings save can request another attempt; snapshots and
    /// prompt saves cannot. Pending values, prompts and uncertainty are retained.
    pub fn resume(host: Box<SettingsHost>) -> (Self, mpsc::Sender<SettingsRequest>) {
        let (send, inbox) = settings_channel();
        (
            Self {
                host,
                inbox,
                restart: Restart::default(),
            },
            send,
        )
    }

    /// Drive the bounded inbox only while this service generation is alive.
    /// The work future includes voice and all owned resource cleanup. Completion
    /// wins over queued requests, so no transaction can begin after it returns.
    /// The status callback is sampled at commit, never cached at bind time.
    pub async fn scope<T>(
        self,
        work: impl Future<Output = T>,
        link_status: impl Fn() -> LinkStatusV1,
    ) -> (T, SettingsCompletion) {
        let Self {
            mut host,
            mut inbox,
            restart,
        } = self;
        let result = restart.scope(async {
            tokio::pin!(work);
            loop {
                tokio::select! {
                    biased;
                    result = &mut work => return result,
                    Some(request) = inbox.recv() => {
                        let mut changed = false;
                        request.commit(|operation| {
                            let save = matches!(operation, SettingsOperation::Save(_));
                            let reply = host.transaction(operation, link_status());
                            changed = save && matches!(&reply, SettingsReply::Saved(result) if result.restart_pending);
                            reply
                        }).await;
                        // Commit has handed the HTTP response to its receipt.
                        // The caller must still drain HTTP before replacement.
                        if changed && !restart.pending() { restart.request(); }
                    }
                }
            }
        }).await;
        // Dropping the inbox closes queued receipts. No detached mutation task
        // remains to outlive this generation, including on future cancellation.
        (
            result,
            SettingsCompletion {
                restart_requested: restart.pending(),
                host,
            },
        )
    }
}

#[cfg(test)]
#[path = "settings_service_tests.rs"]
mod tests;
