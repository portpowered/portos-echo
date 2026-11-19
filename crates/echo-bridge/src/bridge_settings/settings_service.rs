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
    trace: Option<crate::bridge_trace::Trace>,
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
    #[must_use]
    pub fn resume(host: Box<SettingsHost>) -> (Self, mpsc::Sender<SettingsRequest>) {
        let (send, inbox) = settings_channel();
        (
            Self {
                host,
                inbox,
                restart: Restart::default(),
                trace: None,
            },
            send,
        )
    }

    pub(crate) fn with_trace(mut self, trace: crate::bridge_trace::Trace) -> Self {
        self.trace = Some(trace);
        self
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
            mut trace,
        } = self;
        let result = restart
            .scope(async {
                tokio::pin!(work);
                loop {
                    tokio::select! {
                        biased;
                        result = &mut work => return result,
                        Some(request) = inbox.recv() => {
                            request.commit(|operation| {
                                commit(&mut host, &restart, trace.as_mut(), operation, link_status())
                            }).await;
                        }
                    }
                }
            })
            .await;
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

// Record only committed success, with selectors fixed before consuming input.
fn commit(
    host: &mut SettingsHost,
    restart: &Restart,
    trace: Option<&mut crate::bridge_trace::Trace>,
    operation: SettingsOperation,
    link: LinkStatusV1,
) -> SettingsReply {
    let prompt = match &operation {
        SettingsOperation::Prompt(echo_screen_server::PromptKind::Voice, _) => Some("voice"),
        SettingsOperation::Prompt(echo_screen_server::PromptKind::Runner, _) => Some("runner"),
        _ => None,
    };
    let reply = host.transaction(operation, link);
    let SettingsReply::Saved(saved) = &reply else {
        return reply;
    };
    let pending = prompt.is_none() && saved.restart_pending && !restart.pending();
    let recorded = trace.map_or(Ok(()), |trace| {
        let (event, fields) = prompt.map_or_else(
            || {
                (
                    "settings.saved",
                    serde_json::json!({"restart_pending":saved.restart_pending}),
                )
            },
            |prompt| ("prompt.saved", serde_json::json!({"prompt":prompt})),
        );
        trace.record(event, None, fields)?;
        if pending {
            trace.record("settings.reexec_pending", None, serde_json::json!({}))?;
        }
        Ok::<(), crate::Error>(())
    });
    // Persistence has committed even if diagnostics fail. Keep ownership of the
    // pending restart, and refuse to claim a fully successful receipt.
    if pending {
        restart.request();
    }
    if recorded.is_err() {
        return SettingsReply::Persistence(echo_screen_contract::settings_v1::SettingsErrorV1 {
            version: echo_screen_contract::settings_v1::SettingsVersionV1,
            message: "settings: trace unavailable after commit".into(),
        });
    }
    reply
}

#[cfg(test)]
#[path = "settings_service_tests.rs"]
mod tests;
