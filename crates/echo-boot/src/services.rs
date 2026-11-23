use std::{net::IpAddr, time::Duration};

use crate::{HostOps, Log, Result};

const SAMPLE: Duration = Duration::from_secs(1);
const NETWORK_SETTLE: Duration = Duration::from_secs(5);
const INITIAL_LIMIT: Duration = Duration::from_secs(10);
const CLOCK_LIMIT: Duration = Duration::from_secs(6);
const CLOCK_PERIOD: Duration = Duration::from_secs(6 * 60 * 60);

pub enum ClockStatus {
    Running,
    Adjusted,
    Unchanged,
}

#[derive(Default)]
pub(super) struct Services {
    prepared: bool,
    initial_deadline: Option<Duration>,
    core_admitted: bool,
    sample_at: Duration,
    resolvers: Vec<IpAddr>,
    settle_at: Duration,
    clock_started: Option<Duration>,
    clock_due: Duration,
}

impl Services {
    pub(super) fn tick(&mut self, host: &mut impl HostOps) -> bool {
        let now = host.now();
        if !self.prepared {
            self.prepared = true;
            report(host, HostOps::prepare_device);
        }
        report(host, HostOps::poll_device);
        let first = self.initial_deadline.is_none();
        let deadline = *self.initial_deadline.get_or_insert(now + INITIAL_LIMIT);
        if now >= self.sample_at {
            self.sample_at = now + SAMPLE;
            self.sample_network(host, now, first);
        }
        let online = !self.resolvers.is_empty() && now >= self.settle_at;
        self.core_admitted |= online || now >= deadline;
        self.clock(host, now, online);
        self.core_admitted
    }

    fn sample_network(&mut self, host: &mut impl HostOps, now: Duration, first: bool) {
        let mut resolvers = host.network().unwrap_or_else(|stage| {
            host.log(Log::DeviceService(stage));
            Vec::new()
        });
        resolvers.sort_unstable();
        resolvers.dedup();
        if resolvers != self.resolvers {
            self.settle_at = if first { now } else { now + NETWORK_SETTLE };
            self.resolvers = resolvers;
            host.log(Log::Network(!self.resolvers.is_empty()));
        }
    }

    fn clock(&mut self, host: &mut impl HostOps, now: Duration, online: bool) {
        if let Some(started) = self.clock_started {
            if now >= started + CLOCK_LIMIT || !online {
                let stage = if online {
                    "clock sync timed out"
                } else {
                    "clock sync lost network"
                };
                host.log(Log::DeviceService(stage));
                report(host, HostOps::stop_clock);
                self.clock_started = None;
                return;
            }
            match host.poll_clock() {
                Ok(ClockStatus::Running) => (),
                Ok(status) => {
                    self.clock_started = None;
                    if matches!(status, ClockStatus::Adjusted) {
                        report(host, HostOps::write_rtc);
                    }
                }
                Err(stage) => {
                    host.log(Log::DeviceService(stage));
                    report(host, HostOps::stop_clock);
                    self.clock_started = None;
                }
            }
        } else if online && now >= self.clock_due {
            self.clock_due = now + CLOCK_PERIOD;
            match host.start_clock() {
                Ok(()) => self.clock_started = Some(now),
                Err(stage) => host.log(Log::DeviceService(stage)),
            }
        }
    }

    pub(super) fn stop(&mut self, host: &mut impl HostOps) -> Result<()> {
        let [clock, device] = [host.stop_clock(), host.stop_device()];
        clock.and(device)?;
        self.clock_started = None;
        self.prepared = false;
        self.sample_at = Duration::ZERO;
        Ok(())
    }
}

fn report<H: HostOps>(host: &mut H, operation: impl FnOnce(&mut H) -> Result<()>) {
    if let Err(stage) = operation(host) {
        host.log(Log::DeviceService(stage));
    }
}
