mod auth;
mod lifecycle;
mod task;

use tokio::sync::watch;

use crate::{
    MuseLink, ServiceState,
    api::Network,
    clock::{Clock, Wait},
    control::Handler,
    link::OsRandom,
    pairing::Pairing,
    support::{Fake, Pki},
    tasks::Tasks,
    trust,
};

async fn wait_state(link: &MuseLink, initial: ServiceState, expected: ServiceState) {
    let mut state = link.state.clone();
    let (observed, _) = *state.wait_for(|(state, _)| *state != initial).await.unwrap();
    assert_eq!(observed, expected);
}

#[derive(Clone)]
pub(super) struct ManualClock(watch::Sender<u64>);

impl ManualClock {
    pub(super) fn new(now: u64) -> Self {
        Self(watch::Sender::new(now))
    }

    pub(super) fn set(&self, now: u64) {
        self.0.send_replace(now);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> u64 {
        *self.0.borrow()
    }

    fn wait_until(&self, deadline: u64) -> Wait<'_> {
        Box::pin(async move {
            let _ = self.0.subscribe().wait_for(|now| *now >= deadline).await;
        })
    }
}

pub(super) fn start(fake: &Fake, pki: &Pki, clock: &ManualClock) -> MuseLink {
    start_with(fake, pki, clock, Handler::default())
}

pub(super) fn start_with(fake: &Fake, pki: &Pki, clock: &ManualClock, handler: Handler) -> MuseLink {
    let pairing = Pairing::open(&fake.pairing).unwrap();
    let connector = Network::new(trust::client_config(&pki.pem).unwrap()).unwrap();
    MuseLink::spawn_with(
        &tokio::runtime::Handle::current(),
        pairing,
        connector,
        Tasks::open(&fake.task_path(), clock.clone(), OsRandom::new()).unwrap(),
        handler,
    )
}
