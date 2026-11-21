//! Exactly one selected executor owns a delegation; only its caller writes Live.
use super::{Client, bridge_muse_host::Muse};
use crate::bridge_runtime::journey::bridge_live::LivePort;
use crate::{Error, bridge_input::Settings, bridge_mcp::Grant, bridge_trace::Trace};
use echo_contracts::live::LiveEvent;
use std::sync::Arc;
use tokio::time::Instant;

pub(in crate::bridge_runtime) enum Host {
    Client(Box<Client>),
    Muse(Box<Muse>),
}

impl Host {
    pub fn new(settings: &Settings, utc_day: Arc<dyn Fn() -> u64 + Send + Sync>) -> Result<Self, Error> {
        if let Some((agent, config)) = &settings.muse {
            let backend = crate::bridge_muse::Backend::open(config, settings.model_url.is_some())?;
            Ok(Self::Muse(Box::new(Muse::new(
                backend,
                agent.daily_request_cap,
                utc_day,
            ))))
        } else {
            Ok(Self::Client(Box::new(Client::new(settings, utc_day))))
        }
    }

    pub fn grant(&mut self, grant: Arc<Grant>) {
        if let Self::Client(client) = self {
            client.grant(grant);
        }
    }

    pub async fn new_call(&mut self) {
        match self {
            Self::Client(client) => client.new_call().await,
            Self::Muse(muse) => {
                muse.abandon().await;
                muse.new_call();
            }
        }
    }

    pub fn dispatch(&mut self, event: LiveEvent, run: &str, trace: &mut Trace, cutoff: Instant) -> Result<bool, Error> {
        match self {
            Self::Client(client) => client.dispatch(event, run, trace, cutoff),
            Self::Muse(muse) => muse.dispatch(&event, run, trace, cutoff),
        }
    }

    pub async fn ready(&mut self) {
        match self {
            Self::Client(client) => client.ready().await,
            Self::Muse(muse) => muse.ready().await,
        }
    }

    pub fn awaiting_result(&self) -> bool {
        match self {
            Self::Client(client) => client.active.is_some(),
            Self::Muse(muse) => muse.awaiting_result(),
        }
    }

    pub async fn service(
        &mut self,
        call: &mut impl LivePort,
        trace: &mut Trace,
        screen: &crate::bridge_runtime::bridge_screen::Screen,
    ) -> Result<(), Error> {
        match self {
            Self::Client(client) => client.service(call, trace, screen).await,
            Self::Muse(muse) => muse.service(call, trace).await,
        }
    }

    pub async fn abandon(&mut self) -> bool {
        match self {
            Self::Client(client) => client.abandon().await,
            Self::Muse(muse) => muse.abandon().await,
        }
    }

    pub fn cancel(&mut self) {
        match self {
            Self::Client(client) => client.cancel(),
            Self::Muse(muse) => muse.cancel(),
        }
    }
}
