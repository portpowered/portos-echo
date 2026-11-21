//! Runtime-owned dispatch: no detached transactions survive the voice owner.
use super::openai_account::Account;
use super::{SettingsError, SettingsHost, portos_permissions::PortosPermissions, settings_restart::Restart};
use crate::bridge_mcp::Grant;
use echo_config::EffectiveConfig;
use echo_screen_contract::{
    settings_v1::{LinkStatusV1, PromptTextsV1, SettingsErrorV1, SettingsVersionV1},
    settings_v2::{PortosChallengeV2, PortosOperationStateV2, SettingsErrorV2, SettingsVersionV2},
};
use echo_screen_server::{SettingsOperation, SettingsReply, SettingsRequest, settings_channel};
use std::{future::Future, sync::Arc};
use tokio::sync::mpsc;

/// One serialized host and one restart request per service generation.
/// The caller must still await resource cleanup and arbitrate shutdown/kill
/// before attempting exec. Completion here is never proof of replacement.
pub struct SettingsService {
    host: Box<SettingsHost>,
    inbox: mpsc::Receiver<SettingsRequest>,
    restart: Restart,
    trace: Option<crate::bridge_trace::Trace>,
    account: Option<Account>,
}

/// Accepted state survives resource cleanup without reopening the home. A
/// failed exec may remount it alongside the unchanged active configuration.
pub struct SettingsCompletion {
    pub restart_requested: bool,
    pub host: Box<SettingsHost>,
}

#[derive(Default)]
struct GrantView {
    scopes: Option<Vec<String>>,
    operation: Option<(PortosOperationStateV2, Option<PortosChallengeV2>)>,
}

impl GrantView {
    async fn read(owner: Option<&Grant>, snapshot: bool) -> Self {
        if let Some(owner) = owner.filter(|_| snapshot) {
            // Sample the receipt before the grant. A terminal receipt implies
            // the subsequent read can observe the committed grant.
            let operation = Some(owner.screen_operation());
            let scopes = owner.granted_scopes().await;
            return Self { scopes, operation };
        }
        Self::default()
    }
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
        let warnings = config
            .warning
            .into_iter()
            .chain(config.deprecations)
            .map(|error| error.to_string())
            .collect();
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
                account: None,
            },
            send,
        )
    }

    pub(crate) fn without_restart(mut self) -> Self {
        self.host.disable_restart();
        self
    }

    pub(crate) fn permissions(&self) -> PortosPermissions {
        self.host.permissions()
    }

    pub(crate) fn muse_access(&self) -> crate::bridge_muse::Access {
        self.host.muse_access()
    }

    pub(crate) fn with_local_muse(mut self, local: bool) -> Self {
        if local {
            self.host.restrict_muse_to_loopback();
        }
        self
    }

    pub(crate) fn with_trace(mut self, trace: crate::bridge_trace::Trace) -> Self {
        self.trace = Some(trace);
        self
    }

    pub(crate) fn with_account(mut self, account: Account) -> Self {
        self.account = Some(account);
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
        self.scope_with_grant(work, link_status, || None).await
    }

    pub(crate) async fn scope_with_grant<T>(
        self,
        work: impl Future<Output = T>,
        link_status: impl Fn() -> LinkStatusV1,
        current_grant: impl Fn() -> Option<Arc<Grant>>,
    ) -> (T, SettingsCompletion) {
        let Self {
            mut host,
            mut inbox,
            restart,
            mut trace,
            account,
        } = self;
        let result = restart
            .scope(async {
                tokio::pin!(work);
                loop {
                    tokio::select! {
                        biased;
                        result = &mut work => return result,
                        Some(request) = inbox.recv() => {
                            let handle = async {
                                if request.is_muse_probe() {
                                    request.probe_muse(
                                        host.as_mut(), SettingsHost::prepare_muse_probe, SettingsHost::finish_muse_probe,
                                    ).await;
                                    return;
                                }
                                let grant = current_grant();
                                let view = GrantView::read(grant.as_deref(), request.reads_portos()).await;
                                request.commit(|operation| {
                                    if let Some(account) = &account {
                                        match operation {
                                            SettingsOperation::OpenAiSnapshot => return account.snapshot(),
                                            SettingsOperation::OpenAiOperation(request) => return account.start(request),
                                            _ => {}
                                        }
                                    }
                                    let current = current_grant();
                                    let same_owner = grant.as_ref().zip(current.as_ref())
                                        .is_some_and(|(before, now)| Arc::ptr_eq(before, now));
                                    let view = if same_owner { view } else { GrantView::default() };
                                    commit(&mut host, &restart, trace.as_mut(), operation, (link_status(), view, current.as_deref()))
                                }).await;
                            };
                            // Work may itself hold the grant lock during refresh.
                            tokio::select! {
                                biased;
                                result = &mut work => return result,
                                () = handle => {}
                            }
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
    grant: (LinkStatusV1, GrantView, Option<&Grant>),
) -> SettingsReply {
    let (link, view, owner) = grant;
    let pack_only = matches!(&operation, SettingsOperation::PackSelection(..));
    let prompt = match &operation {
        SettingsOperation::Prompt(echo_screen_server::PromptKind::Voice, _) => Some("voice"),
        SettingsOperation::Prompt(echo_screen_server::PromptKind::Runner, _) => Some("runner"),
        _ => None,
    };
    let legacy_v2 = matches!(
        &operation,
        SettingsOperation::SaveV2(patch) if super::settings_host::settings_v2::legacy_only(&patch.settings)
    );
    let legacy_v1 = legacy_v1_cap_only(&operation);
    let mut reply = host.transaction_with_grant(operation, link, view.scopes, owner);
    if legacy_v2 {
        return reply;
    }
    if let (SettingsReply::SnapshotV2(snapshot), Some((state, challenge))) = (&mut reply, view.operation) {
        snapshot.portos.operation_state = state;
        snapshot.portos.challenge = challenge;
    }
    let (restart_pending, v2) = match &reply {
        SettingsReply::Saved(saved) | SettingsReply::PackSaved(saved, _, _) => (saved.restart_pending, false),
        SettingsReply::SavedV2(saved) => (saved.restart_pending, true),
        _ => return reply,
    };
    let pending = !pack_only && !legacy_v1 && prompt.is_none() && restart_pending && !restart.pending();
    let recorded = trace.map_or(Ok(()), |trace| {
        let (event, fields) = prompt.map_or_else(
            || ("settings.saved", serde_json::json!({"restart_pending":restart_pending})),
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
        let message = "settings: trace unavailable after commit".into();
        return if v2 {
            SettingsReply::PersistenceV2(SettingsErrorV2 {
                version: SettingsVersionV2,
                message,
            })
        } else {
            SettingsReply::Persistence(SettingsErrorV1 {
                version: SettingsVersionV1,
                message,
            })
        };
    }
    reply
}

fn legacy_v1_cap_only(operation: &SettingsOperation) -> bool {
    matches!(
        operation,
        SettingsOperation::Save(patch)
            if patch.settings.voice.is_none() && patch.settings.codex.is_none()
                && patch.settings.agent.as_ref().is_some_and(|agent| {
                    agent.daily_request_cap.is_none() && agent.max_tool_calls.is_some()
                })
    )
}

#[cfg(test)]
#[path = "settings_service_tests.rs"]
mod tests;
