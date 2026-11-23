//! Presentation lifetime encloses authenticated device and conversation cleanup.
mod lifecycle;
use super::{
    HostOptions, ServeOptions,
    retained_link::Retained,
    voice_host::with_capture,
    voice_recovery::{Round, Stage, stopped, supervise},
    voice_view::View,
};
use crate::bridge_settings::{
    SettingsCompletion, SettingsHost, SettingsService, openai_account::Account, portos_permissions::PortosPermissions,
    settings_restart::replacement_pending,
};
use crate::{Error, bridge_health::Health, bridge_runtime::bridge_screen::Screen, bridge_trace::Trace};
use echo_config::{LiveMode, ScreenConfig};
use echo_link::LinkConnector;
use echo_media::MediaPeer;
use echo_screen_contract::settings_v1::LinkStatusV1;
use echo_screen_server::SettingsRequest;
pub(super) use lifecycle::execute_owned;
use std::{cell::Cell, future::Future, net::SocketAddr, path::Path, pin::Pin};
use tokio::{sync::OnceCell, time::Instant};

pub struct RuntimeOptions<'a> {
    pub runner_skills: Option<&'a super::RunnerSkills>,
    pub host: HostOptions<'a>,
    pub voice: ServeOptions<'a>,
    pub screen: Option<&'a ScreenConfig>,
    pub settings_warning: Option<&'a str>,
    pub health: SocketAddr,
    pub trace: &'a Path,
    pub delegation: Option<&'a echo_config::BridgeConfig>,
    pub utc_day: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
    pub mcp_epoch: Option<u64>,
}

/// Cleanup-complete admission plus the latest accepted settings owner. Keep the
/// active config separately: persisted pending values must not become active if
/// exec fails. Final EOF/kill arbitration and actual replacement belong to main.
#[derive(Default)]
pub struct RuntimeCompletion {
    pub restart_requested: bool,
    pub settings: Option<Box<SettingsHost>>,
}

impl RuntimeOptions<'_> {
    /// Device stalled-progress protection exceeds either waiting or active work.
    /// It is separate from controller idle policy and between-wakes call reuse.
    pub(super) fn capture_failsafe_seconds(&self) -> u32 {
        let Some(config) = self.delegation else { return 30 };
        let seconds = u64::from(config.voice.session_max_seconds)
            .max(self.voice.session.response_timeout.as_secs())
            .saturating_add(self.voice.session.played_timeout.as_secs());
        u32::try_from(seconds.min(i32::MAX as u64)).unwrap_or(i32::MAX as u32)
    }
}

struct SettingsLink<'a> {
    health: &'a Health,
    portos: bool,
    permissions: Option<PortosPermissions>,
    muse_access: Option<crate::bridge_muse::Access>,
    account: Option<Account>,
    retained: OnceCell<Option<crate::bridge_mcp::Runtime>>,
}

impl<'a> SettingsLink<'a> {
    fn new(
        config: Option<&echo_config::BridgeConfig>,
        health: &'a Health,
        permissions: Option<PortosPermissions>,
        muse_access: Option<crate::bridge_muse::Access>,
        account: Option<Account>,
    ) -> Self {
        let portos = config.is_some_and(|config| {
            config
                .mcp
                .server
                .first()
                .is_some_and(|server| server.auth == echo_config::McpAuth::PortosDeviceCode)
        });
        if portos {
            health.state.send_modify(|state| state.mcp = "unlinked");
        }
        Self {
            health,
            portos,
            permissions,
            muse_access,
            account,
            retained: OnceCell::new(),
        }
    }

    fn tools(&self, options: &RuntimeOptions<'_>) -> Option<super::voice_tools::Tools> {
        options.delegation.map(|config| {
            super::voice_tools::Tools::new(config, options.utc_day.clone())
                .with_muse(config, options.host.routes.is_some())
                .with_muse_access(self.muse_access.clone())
                .with_portos_permissions(self.permissions.clone())
                .with_runner_skills(options.runner_skills)
        })
    }

    async fn retain_portos(
        &self,
        view: &mut View<'_>,
        options: &RuntimeOptions<'_>,
    ) -> Result<Option<std::sync::Arc<crate::bridge_mcp::Grant>>, Error> {
        let Some(config) = options.delegation.filter(|_| self.portos && self.account.is_some()) else {
            return Ok(None);
        };
        let runtime = self
            .retained
            .get_or_try_init(|| view.open_portos(config, options.mcp_epoch))
            .await?;
        let grant = runtime.as_ref().map(|runtime| runtime.grant.clone());
        self.health.set_mcp(grant.clone())?;
        Ok(grant)
    }

    async fn open_tools(
        &self,
        view: &mut View<'_>,
        options: &RuntimeOptions<'_>,
        tools: &mut Option<super::voice_tools::Tools>,
    ) -> Result<(), Error> {
        if let (Some(config), Some(tools)) = (options.delegation, tools) {
            if let Some(grant) = self.retain_portos(view, options).await? {
                tools.share_grant(grant);
            }
            view.open_tools(tools, config, options.mcp_epoch).await?;
            self.health.set_mcp(tools.grant())?;
        }
        Ok(())
    }

    fn release_unretained(&self) -> Result<(), Error> {
        if self.retained.get().is_none() {
            self.health.set_mcp(None)?;
        }
        Ok(())
    }

    fn status(&self) -> LinkStatusV1 {
        if self.portos {
            self.health
                .mcp_grant()
                .map_or(LinkStatusV1::LinkRequired, |grant| grant.link_status())
        } else {
            LinkStatusV1::Unconfigured
        }
    }
}

/// Compose the Codex voice service with the existing screen, health and trace
/// owners. Authentication precedes device readiness. Only negotiated media can
/// publish Connected; played settlement precedes Ready. All device/call/process
/// cleanup finishes while presentation remains owned.
///
/// Configured delegation retains policy, consent and the effect ledger across calls.
///
/// After first readiness no failure ends this owner: a failed generation is
/// traced, shown on health and screen, and replaced after bounded backoff while
/// the device link is kept (see `voice_recovery`).
///
/// # Errors
/// Fixed startup, credential and control failures.
pub async fn execute<C: LinkConnector, P: MediaPeer>(
    options: RuntimeOptions<'_>,
    connector: C,
    mut peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
) -> Result<(), Error> {
    let trace = Trace::open_for_voice(options.trace, options.host.mode, None, options.delegation)?;
    Box::pin(execute_owned(
        options,
        connector,
        &mut peer,
        cancelled,
        None,
        (trace, None),
    ))
    .await
    .map(|_| ())
}

/// Mount the bounded authorized settings owner for this runtime generation.
/// Completion requests replacement only after call/device/process, health
/// and HTTP cleanup. The caller must still arbitrate final EOF/kill, drop the
/// runtime, and perform exec or recover the old active configuration on failure.
/// Link status is sampled at commit from this runtime's live grant owner.
/// Accepted settings state is returned without reopening disk for recovery.
///
/// # Errors
/// Uses the same startup, service and cleanup failures as `execute`.
pub async fn execute_with_settings<C: LinkConnector, P: MediaPeer>(
    options: RuntimeOptions<'_>,
    connector: C,
    mut peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    settings: (SettingsService, tokio::sync::mpsc::Sender<SettingsRequest>),
) -> Result<RuntimeCompletion, Error> {
    let trace = Trace::open_for_voice(options.trace, options.host.mode, None, options.delegation)?;
    Box::pin(execute_owned(
        options,
        connector,
        &mut peer,
        cancelled,
        Some(settings),
        (trace, None),
    ))
    .await
}

async fn execute_runtime<C: LinkConnector, P: MediaPeer>(
    options: RuntimeOptions<'_>,
    connector: C,
    mut peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    settings: Option<(SettingsService, tokio::sync::mpsc::Sender<SettingsRequest>)>,
    mut trace: Trace,
) -> Result<RuntimeCompletion, Error> {
    super::prompts::publish(&mut trace)?;
    let (service, send) = settings.map_or((None, None), |(service, send)| (Some(service), Some(send)));
    let camera = cfg!(target_os = "android") && options.delegation.is_some_and(|config| config.camera.enabled);
    let mut screen = Screen::bind_for_runtime(options.screen, send, camera).await?;
    screen.download_packs(options.delegation, &trace);
    screen.settings_warning(options.settings_warning, &mut trace)?;
    let health = match Health::bind(options.health).await {
        Ok(health) => health,
        Err(error) => {
            let _ = close_screen(&mut screen, &mut trace, false).await;
            return Err(error);
        }
    };
    let account = service
        .as_ref()
        .map(|_| Account::new(options.host.mode, screen.presentation.clone(), trace.clone()));
    let recovery = account.clone().filter(|_| options.host.mode == LiveMode::Chatgpt);
    screen.supervise_actions(health.state.subscribe(), recovery.clone());
    let service = service.map(|service| {
        service
            .with_local_muse(options.host.routes.is_some())
            .with_trace(trace.clone())
    });
    let world = trace.world();
    if let Some(config) = options.delegation {
        world.configure_session(echo_core_api::SessionKnobs {
            expiry: std::time::Duration::from_secs(u64::from(config.voice.speech_start_expiry_seconds)),
            local_cut: config.voice.local_barge_cut,
        })?;
    }
    let mut view = View::new(&screen, &health, &mut trace).with_screen_account(recovery);
    let requested = Cell::new(false);
    let mut link = SettingsLink::new(
        options.delegation,
        &health,
        service.as_ref().map(SettingsService::permissions),
        service.as_ref().map(SettingsService::muse_access),
        account.clone(),
    );
    let lips = screen.lips(options.screen);
    let work = Box::pin(crate::bridge_world::scope(
        world,
        lips,
        work(options, connector, &mut peer, cancelled, &mut view, &requested, &link),
    ));
    let (result, settings) = if let Some((service, account)) = service.zip(account) {
        let (result, completion) = Box::pin(
            service
                .with_account(account)
                .with_screen_camera(&screen)
                .scope_with_grant(work, || link.status(), || link.health.mcp_grant()),
        )
        .await;
        (result, Some(completion))
    } else {
        (work.await, None)
    };
    if let Some(Some(mut portos)) = link.retained.take() {
        portos.close().await;
    }
    finish(screen, health, trace, result, settings, requested.get()).await
}

async fn finish(
    mut screen: Screen,
    health: Health,
    mut trace: Trace,
    result: Result<(), Error>,
    settings: Option<SettingsCompletion>,
    requested: bool,
) -> Result<RuntimeCompletion, Error> {
    let restart = settings.as_ref().is_some_and(|settings| settings.restart_requested) && result.is_ok() && !requested;
    let presentation = if restart {
        screen
            .publish_confirmed(echo_screen_contract::UiState::Ready {
                message: "Restarting bridge...".into(),
            })
            .await
            .and_then(|()| trace.record("settings.restart_prepared", None, serde_json::json!({})))
    } else {
        Ok(())
    };
    health.close().await;
    let health_receipt = trace.record("health.released", None, serde_json::json!({}));
    // A trace failure must not skip the remaining presentation cleanup.
    let cleanup = close_screen(&mut screen, &mut trace, !requested && result != Err(Error::Control)).await;
    result
        .and(presentation)
        .and(cleanup)
        .and(health_receipt)
        .map(|()| RuntimeCompletion {
            restart_requested: restart,
            settings: settings.map(|settings| settings.host),
        })
}

async fn close_screen(screen: &mut Screen, trace: &mut Trace, graceful: bool) -> Result<(), Error> {
    let acquired = screen.address().is_some();
    let dropped = if graceful {
        screen.close_gracefully().await?
    } else {
        screen.close().await?;
        0
    };
    if acquired {
        trace.record("screen.released", None, serde_json::json!({"dropped_clients":dropped}))?;
    }
    Ok(())
}

async fn work<C: LinkConnector, P: MediaPeer>(
    options: RuntimeOptions<'_>,
    connector: C,
    peer: impl AsyncFnMut() -> Result<P, Error>,
    cancelled: impl Future<Output = ()>,
    view: &mut View<'_>,
    requested: &Cell<bool>,
    grant: &SettingsLink<'_>,
) -> Result<(), Error> {
    view.startup()?;
    let cancelled = async {
        cancelled.await;
        requested.set(true);
    };
    tokio::pin!(cancelled);
    let link = Retained::new(connector);
    let mut stage = Live {
        budget: options.host.deadline.saturating_duration_since(Instant::now()),
        options,
        peer,
        view,
        requested,
        grant,
    };
    let Some(account) = &grant.account else {
        let result = supervise(&mut stage, &link, cancelled.as_mut()).await;
        settled(stage.view, &result, requested.get()).await?;
        if result == Err(Error::Authorization) && !requested.get() {
            cancelled.await;
        }
        return result;
    };
    loop {
        let result = account.scope(supervise(&mut stage, &link, cancelled.as_mut())).await;
        settled(stage.view, &result, requested.get()).await?;
        let signed_out = result == Err(Error::Authorization);
        if signed_out {
            // Settings can still operate the same Port OS owner while Live is signed out.
            let _ = grant.retain_portos(stage.view, &stage.options).await;
        } else if result.is_err() {
            return result;
        }
        if requested.get()
            || !account
                .handoff(&stage.options.host, cancelled.as_mut(), signed_out)
                .await
        {
            return if replacement_pending() && !requested.get() {
                Ok(())
            } else {
                result
            };
        }
    }
}

async fn settled(view: &mut View<'_>, result: &Result<(), Error>, requested: bool) -> Result<(), Error> {
    match result {
        Err(error) => view.failed(error).await,
        Ok(()) => view.closed(if requested { "shutdown" } else { "stopped" }),
    }
}

/// The production stage: each generation is one Codex child, its account
/// admission, the kept device link, MCP tools and the wake service.
struct Live<'a, 'v, 'w, F> {
    options: RuntimeOptions<'a>,
    peer: F,
    view: &'v mut View<'w>,
    requested: &'v Cell<bool>,
    grant: &'v SettingsLink<'v>,
    /// Startup allowance, granted afresh to every generation.
    budget: std::time::Duration,
}

impl<C: LinkConnector, P: MediaPeer, F: AsyncFnMut() -> Result<P, Error>> Stage<C> for Live<'_, '_, '_, F> {
    async fn generation(&mut self, link: &Retained<C>, cancelled: Pin<&mut impl Future<Output = ()>>) -> Round {
        let (options, view, peer, requested) = (&self.options, &mut *self.view, &mut self.peer, self.requested);
        let grant = self.grant;
        let termination = super::voice_kill::Termination::default();
        let (ready, served) = (Cell::new(None), Cell::new(false));
        let mut host = options.host.clone();
        host.deadline = Instant::now() + self.budget;
        let mut voice = options.voice.clone();
        let recovering = grant.health.state.borrow().relink_required && grant.account.is_some();
        voice.session.muted = link
            .muted()
            .unwrap_or_else(|| voice.session.muted || (recovering && view.screen.muted().unwrap_or(true)));
        let result = with_capture(
            host,
            link,
            options.capture_failsafe_seconds(),
            cancelled,
            async |io, client, stopping| {
                let mut tools = grant.tools(options);
                let setup = grant.open_tools(view, options, &mut tools);
                let prepared = tokio::select! {
                    biased;
                    () = stopped(stopping.clone()) => Err(Error::Control),
                    result = tokio::time::timeout(std::time::Duration::from_secs(15), setup) => result.unwrap_or(Err(Error::Deadline)),
                };
                let setup_trace = view.setup_trace();
                let earcon = options.delegation.is_some_and(|config| config.wake_earcon);
                let result = match prepared {
                    Ok(()) => {
                        let observe = |notice| {
                            progress(&notice, &ready, &served);
                            termination.observe(&notice);
                            view.observe(notice)
                        };
                        let remote = super::remote_admission::Options::retained(
                            options.delegation.is_some_and(|config| config.portos_endpoint.remote_mic),
                        );
                        let context = (tools.as_mut(), Some(setup_trace), earcon, Some(&termination), Some(remote));
                        super::voice_admission::service(client, io, voice, peer, stopped(stopping), observe, context).await
                    }
                    Err(error) => Err(error),
                };
                termination.returned(&result, requested.get());
                if let Some(tools) = &mut tools {
                    tools.close().await;
                }
                result.and(grant.release_unretained())
            },
        )
        .await;
        // with_capture has now awaited call/media and process shutdown.
        let result = termination.finish(result, requested.get(), options.voice.kill_path, &mut |notice| {
            view.observe(notice)
        });
        Round {
            result,
            ready: ready.get(),
            served: served.get(),
            killed: termination.killed(),
        }
    }

    fn requested(&self) -> bool {
        self.requested.get()
    }

    async fn recovering(&mut self, error: &Error, attempt: u32, delay: std::time::Duration, linked: bool) {
        // Reporting is best effort: a failed report must not end the retry.
        let _ = self.view.recovering(error, (attempt, delay), linked);
    }

    fn device(&mut self, event: echo_link::LinkEvent<()>) {
        let notice = super::ServeNotice::Device(event);
        if let Some(endpoint) = crate::bridge_endpoint::voice::Voice::current() {
            let _ = endpoint.observe(&notice);
        }
        let _ = self.view.observe(notice);
    }
}

fn progress(notice: &super::ServeNotice, ready: &Cell<Option<Instant>>, served: &Cell<bool>) {
    use super::{ServeNotice, SessionNotice};
    match notice {
        ServeNotice::Ready if ready.get().is_none() => ready.set(Some(Instant::now())),
        ServeNotice::Session(SessionNotice::Connected | SessionNotice::Completed(_)) => served.set(true),
        _ => (),
    }
}

#[cfg(test)]
#[path = "recovery/runtime_tests.rs"]
mod tests;
