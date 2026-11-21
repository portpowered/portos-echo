//! Offline configuration admission, then explicitly nominated credential loading.
use super::{HostOptions, RunnerSkills, RuntimeOptions, ServeOptions, SessionOptions};
use crate::{Error, bridge_codex};
use echo_codex::DynamicTool;
use echo_config::{BridgeConfig, EffectiveConfig, LiveMode};
use echo_contracts::secret::Secret;
use echo_link::LinkConnector;
use echo_media::MediaPeer;
use std::{future::Future, io::Read, net::SocketAddr, path::PathBuf, time::Duration};
use tokio::time::Instant;

/// Own paths whose borrows span the authenticated process and device lifetime.
/// Preparation never opens credentials, resolves a binary or creates a home.
pub struct Configured {
    config: BridgeConfig,
    runner_skills: RunnerSkills,
    kill_path: PathBuf,
    trace: PathBuf,
    health: SocketAddr,
    routes: Option<crate::bridge_operator::LocalRoutes>,
    utc_day: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>,
    mcp_epoch: Option<u64>,
    settings_warning: Option<String>,
    startup: Option<Box<EffectiveConfig>>,
    settings_restart_allowed: bool,
    time_servers: Vec<String>,
}

impl Configured {
    /// Admit the Codex backend without acquiring any runtime resource.
    /// # Errors
    /// Missing home or a backend outside this voice owner's responsibility.
    pub fn new(config: BridgeConfig, trace: PathBuf, health: SocketAddr) -> Result<Self, Error> {
        let runner_skills = RunnerSkills::load(&config)?;
        Self::admit(config, trace, health, runner_skills)
    }

    pub(super) fn admit(
        config: BridgeConfig,
        trace: PathBuf,
        health: SocketAddr,
        runner_skills: RunnerSkills,
    ) -> Result<Self, Error> {
        if config.codex.home.is_none() {
            return Err(Error::CodexHomeRequired);
        }
        Ok(Self {
            kill_path: config.audit.log_dir.join("bridge-kill"),
            // Only servers the operator named: the executable adds the public defaults.
            time_servers: config.clock.servers.clone().unwrap_or_default(),
            config,
            runner_skills,
            trace,
            health,
            routes: None,
            mcp_epoch: None,
            settings_warning: None,
            startup: None,
            settings_restart_allowed: true,
            utc_day: std::sync::Arc::new(|| {
                crate::bridge_time::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_secs() / 86_400)
            }),
        })
    }

    /// Retain one offline startup read for settings merges and warning display.
    /// Use `execute_with_settings` to mount this optional home-backed capability.
    /// # Errors
    /// Uses the same offline backend admission as `new`.
    pub fn from_effective(startup: EffectiveConfig, trace: PathBuf, health: SocketAddr) -> Result<Self, Error> {
        Ok(Self::new(startup.config.clone(), trace, health)?.with_effective(Box::new(startup)))
    }

    pub(super) fn with_effective(mut self, startup: Box<EffectiveConfig>) -> Self {
        self.settings_warning = startup.warning.as_ref().map(ToString::to_string);
        self.startup = Some(startup);
        self
    }

    /// Route only the owned child to validated local fakes; no global mutation.
    #[must_use]
    pub fn with_routes(mut self, routes: crate::bridge_operator::LocalRoutes) -> Self {
        self.routes = Some(routes);
        self
    }

    #[must_use]
    pub fn with_clock(mut self, utc_day: std::sync::Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.utc_day = utc_day;
        self
    }

    /// Retain the effective loader's safe overlay diagnostic for idle presentation.
    #[must_use]
    pub fn with_settings_warning(mut self, warning: Option<String>) -> Self {
        self.settings_warning = warning;
        self
    }

    /// Measure against the public default servers unless `[clock] servers` names its own.
    pub(super) fn with_default_time_servers(mut self) -> Self {
        self.time_servers = self.config.clock.effective_servers();
        self
    }

    // Held for a generation: the first measurement is in before the device is admitted.
    async fn watch_time(&self) -> crate::bridge_time::Watch {
        let mode = match self.config.live.mode {
            LiveMode::Chatgpt => "chatgpt",
            LiveMode::ApiKey => "api_key",
        };
        let trace = if self.time_servers.is_empty() {
            None
        } else {
            crate::bridge_trace::Trace::open(&self.trace, mode).ok()
        };
        let interval = Duration::from_secs(u64::from(self.config.clock.interval_seconds));
        crate::bridge_time::start(self.time_servers.clone(), interval, trace).await
    }

    pub(crate) fn with_mcp_epoch(mut self, epoch: Option<u64>) -> Self {
        self.mcp_epoch = epoch;
        self
    }

    pub(super) fn without_settings_restart(mut self) -> Self {
        self.settings_restart_allowed = false;
        self
    }

    /// Run configured voice ownership with caller-owned tools and media factory.
    /// The executable's connector/factory and controlled component ports share
    /// this boundary. Authentication always precedes device admission.
    /// # Errors
    /// Fixed credential, process, call, device and cleanup categories only.
    pub async fn execute<C: LinkConnector, P: MediaPeer>(
        self,
        connector: C,
        peer: impl AsyncFnMut() -> Result<P, Error>,
        tools: Vec<DynamicTool>,
        cancelled: impl Future<Output = ()>,
    ) -> Result<(), Error> {
        Box::pin(super::prompts::scope(
            self.config.home.as_ref().map(|home| home.dir.as_path()),
            async {
                let api_key = self.api_key()?;
                let _time = self.watch_time().await;
                let options = self.options(api_key.as_ref(), tools);
                super::execute(options, connector, peer, cancelled).await
            },
        ))
        .await
    }

    /// Mount settings using the retained base/overlay and already admitted prompts.
    /// No home or no retained startup keeps the original inactive HTTP surface.
    /// Completion retains pending settings and requests replacement after cleanup;
    /// the caller still owns
    /// final shutdown/kill arbitration, runtime destruction, exec and recovery.
    /// # Errors
    /// Adds safe settings-home failure to the normal configured runtime errors.
    pub async fn execute_with_settings<C: LinkConnector, P: MediaPeer>(
        mut self,
        connector: C,
        peer: impl AsyncFnMut() -> Result<P, Error>,
        tools: Vec<DynamicTool>,
        cancelled: impl Future<Output = ()>,
    ) -> Result<super::RuntimeCompletion, Error> {
        self.execute_generation(connector, peer, tools, cancelled, None).await
    }

    /// Run another generation under the original active configuration after a
    /// failed exec. Accepted edits remain pending and the surface stays usable.
    /// Call only after all previous runtime resources have been released.
    ///
    /// # Errors
    /// Returns fixed startup, credential, runtime and cleanup categories.
    pub async fn execute_generation<C: LinkConnector, P: MediaPeer>(
        &mut self,
        connector: C,
        peer: impl AsyncFnMut() -> Result<P, Error>,
        tools: Vec<DynamicTool>,
        cancelled: impl Future<Output = ()>,
        retained: Option<Box<crate::bridge_settings::SettingsHost>>,
    ) -> Result<super::RuntimeCompletion, Error> {
        let startup = self.startup.take().filter(|startup| startup.config.home.is_some());
        let prompts = retained.as_ref().map(|host| host.prompts());
        Box::pin(super::prompts::scope_retained(
            self.config.home.as_ref().map(|home| home.dir.as_path()),
            prompts,
            async {
                let api_key = self.api_key()?;
                let _time = self.watch_time().await;
                let settings = if let Some(host) = retained {
                    Some(crate::bridge_settings::SettingsService::resume(host))
                } else {
                    startup
                        .map(|startup| {
                            crate::bridge_settings::SettingsService::open(*startup, super::prompts::accepted()?)
                                .map_err(|_| Error::Prompt("home.dir: settings unavailable"))
                        })
                        .transpose()?
                };
                let options = self.options(api_key.as_ref(), tools);
                if let Some((service, send)) = settings {
                    let service = if self.settings_restart_allowed {
                        service
                    } else {
                        service.without_restart()
                    };
                    Box::pin(super::execute_with_settings(
                        options,
                        connector,
                        peer,
                        cancelled,
                        (service, send),
                    ))
                    .await
                } else {
                    Box::pin(super::execute(options, connector, peer, cancelled))
                        .await
                        .map(|()| super::RuntimeCompletion::default())
                }
            },
        ))
        .await
    }

    /// Record one failed OS replacement, keeping the original active config.
    /// The caller passes the returned host into `execute_generation` for an
    /// explicit remount; this never reads pending settings from disk.
    ///
    /// # Errors
    /// A trace write failure is reported without OS paths or error details.
    pub fn replacement_failed(&mut self, host: &mut crate::bridge_settings::SettingsHost) -> Result<(), Error> {
        self.settings_warning = Some(crate::bridge_settings::RESTART_FAILED.into());
        host.replacement_failed();
        let mode = match self.config.live.mode {
            LiveMode::Chatgpt => "chatgpt",
            LiveMode::ApiKey => "api_key",
        };
        crate::bridge_trace::Trace::open(&self.trace, mode)?.record(
            "settings.reexec_failed",
            None,
            serde_json::json!({"reason":"exec"}),
        )
    }

    /// Record an actual replacement admission after the final EOF/kill gate.
    /// # Errors
    /// Refuses replacement when its trace receipt cannot be written.
    pub(super) fn replacement_attempt(&self) -> Result<(), Error> {
        let mode = match self.config.live.mode {
            LiveMode::Chatgpt => "chatgpt",
            LiveMode::ApiKey => "api_key",
        };
        crate::bridge_trace::Trace::open(&self.trace, mode)?.record(
            "settings.reexec_attempt",
            None,
            serde_json::json!({}),
        )
    }

    fn api_key(&self) -> Result<Option<Secret<String>>, Error> {
        if self.config.live.mode == LiveMode::Chatgpt {
            return Ok(None);
        }
        let file = std::fs::File::open(&self.config.live.credential_file).map_err(|_| Error::Authorization)?;
        let mut bytes = Vec::new();
        file.take(16_385)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Authorization)?;
        if bytes.len() > 16_384 {
            return Err(Error::Authorization);
        }
        let text = String::from_utf8(bytes).map_err(|_| Error::Authorization)?;
        let key = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(&text);
        if key.trim().is_empty() || key.chars().any(char::is_control) {
            return Err(Error::Authorization);
        }
        Ok(Some(Secret::new(key.into())))
    }

    fn options<'a>(&'a self, api_key: Option<&'a Secret<String>>, tools: Vec<DynamicTool>) -> RuntimeOptions<'a> {
        RuntimeOptions {
            runner_skills: Some(&self.runner_skills),
            host: HostOptions {
                config: &self.config.codex,
                mode: self.config.live.mode,
                routes: self.routes.as_ref(),
                api_key,
                deadline: Instant::now() + Duration::from_secs(30),
            },
            voice: ServeOptions {
                kill_path: Some(&self.kill_path),
                call: bridge_codex::Options {
                    voice: self.config.codex_voice(),
                    instructions: echo_config::RUNNER_INSTRUCTIONS,
                    prompt: echo_config::LIVE_INSTRUCTIONS,
                    model: self.config.codex_model(),
                    effort: self.config.codex.delegate_effort.as_deref(),
                    fast: self.config.codex.delegate_fast,
                    time_zone: self.config.codex.time_zone.as_deref(),
                    web_search: self.config.codex.web_search,
                },
                tools,
                session: SessionOptions {
                    // Duplex runs stay open after a reply until this listen
                    // deadline; live.session_idle_seconds is between-wakes reuse.
                    listen_timeout: self
                        .config
                        .voice
                        .session_listen_enabled
                        .then(|| Duration::from_secs(u64::from(self.config.voice.session_listen_seconds))),
                    session_max: Duration::from_secs(u64::from(self.config.voice.session_max_seconds)),
                    idle: Duration::from_secs(self.config.live.session_idle_seconds),
                    response_timeout: super::voice_session::NO_ANSWER_TIMEOUT,
                    played_timeout: Duration::from_secs(5),
                    reply_lead_ms: self.config.audio.reply_lead_ms,
                    muted: false,
                },
                connect_timeout: Duration::from_secs(15),
            },
            screen: Some(&self.config.screen),
            settings_warning: self.settings_warning.as_deref(),
            health: self.health,
            trace: &self.trace,
            delegation: Some(&self.config),
            utc_day: self.utc_day.clone(),
            mcp_epoch: self.mcp_epoch,
        }
    }
}

/// The session tools are the bridge's own and so is their guidance. It joins
/// whatever base prompts were admitted, built in or from the owner's files,
/// so a prompt file that predates a tool cannot leave that tool unused. The
/// voice's guidance leads its prompt: placed after a base prompt that says to
/// avoid hand-offs, the Live model on the Echo answered every goodbye with a
/// farewell and never handed it off, so end_session was never called.
pub(super) fn session_guidance((runner, voice): (String, String), advertised: bool) -> (String, String) {
    if !advertised {
        return (runner, voice);
    }
    // The admitted text stays byte for byte; the guidance only joins it.
    (
        format!("{runner}\n\n{}", echo_config::RUNNER_SESSION_GUIDANCE),
        format!("{}\n\n{voice}", echo_config::LIVE_SESSION_GUIDANCE),
    )
}

/// Append a call-local snapshot. Std has no portable local-zone discovery.
pub(crate) fn call_instructions(prefix: &str, now: std::time::SystemTime, zone: Option<&str>) -> Result<String, Error> {
    Ok(format!("{prefix}\n{}", time_context("Call-start", now, zone)?))
}

#[cfg(test)]
tokio::task_local! {
    pub(crate) static TEST_WAKE_TIME: std::cell::Cell<std::time::SystemTime>;
}

pub(super) fn wake_time() -> std::time::SystemTime {
    #[cfg(test)]
    if let Ok(now) = TEST_WAKE_TIME.try_with(std::cell::Cell::get) {
        return now;
    }
    crate::bridge_time::now()
}

pub(crate) fn wake_time_context(zone: Option<&str>) -> Result<String, Error> {
    let context = time_context("Wake-time", wake_time(), zone)?;
    Ok(format!(
        "{context}\nUse this newer time snapshot for time answers. This is quiet context. Do not read it aloud or reply to it."
    ))
}

fn time_context(label: &str, now: std::time::SystemTime, zone: Option<&str>) -> Result<String, Error> {
    let seconds = match now.duration_since(std::time::UNIX_EPOCH) {
        Ok(value) => i64::try_from(value.as_secs()).map_err(|_| Error::Input)?,
        Err(value) => {
            let value = value.duration();
            -i64::try_from(value.as_secs()).map_err(|_| Error::Input)? - i64::from(value.subsec_nanos() != 0)
        }
    };
    if !(-62_167_219_200..=253_402_300_799).contains(&seconds) {
        return Err(Error::Input);
    }
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = calendar(days);
    let weekday = match (days + 4).rem_euclid(7) {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        _ => "Saturday",
    };
    let zone = zone.map_or_else(
        || "local offset unavailable".into(),
        |zone| format!("operator time zone: {zone}; answer time questions in {zone}"),
    );
    Ok(format!(
        "[{label} context: {year:04}-{month:02}-{day:02} {weekday} {:02}:{:02}:{:02} UTC+00:00; {zone}]",
        time / 3600,
        time / 60 % 60,
        time % 60
    ))
}

// March-based Gregorian eras: each 400-year era contains exactly 146097 days.
fn calendar(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = march_month + if march_month < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
#[path = "tests/voice_config_tests.rs"]
pub(super) mod tests;
