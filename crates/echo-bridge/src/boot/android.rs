use std::{
    io::Write,
    net::IpAddr,
    process::Stdio,
    time::{Duration, Instant},
};

use echo_boot::{ChildStatus, Digest, Family, HostOps, Identity, Log, Result, Role, Selection, Slot, Stop};
use tokio::{
    runtime::{Builder, Runtime},
    signal::unix::{Signal, SignalKind, signal},
};

use super::{
    firewall_io::FirewallIo,
    launcher,
    layout_io::{self, Layout},
    processes::RoleChild,
};

pub(super) struct Android {
    layout: Option<Layout>,
    firewall: FirewallIo,
    origin: Instant,
    device: Option<RoleChild>,
    core: Option<RoleChild>,
    signals: Runtime,
    interrupt: Signal,
    terminate: Signal,
    shutdown: bool,
}

impl Android {
    pub(super) fn open(supervise: bool) -> Result<Self> {
        let layout = if supervise {
            rustix::process::set_child_subreaper(Some(rustix::process::Pid::INIT))
                .map_err(|_| "enable role descendant reaping")?;
            Some(Layout::open()?)
        } else {
            None
        };
        let signals = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "open boot signal loop")?;
        let (interrupt, terminate) = {
            let _runtime = signals.enter();
            (signal(SignalKind::interrupt()), signal(SignalKind::terminate()))
        };
        Ok(Self {
            layout,
            firewall: FirewallIo::default(),
            origin: Instant::now(),
            device: None,
            core: None,
            signals,
            interrupt: interrupt.map_err(|_| "watch boot interrupt")?,
            terminate: terminate.map_err(|_| "watch boot termination")?,
            shutdown: false,
        })
    }

    fn child(&mut self, role: Role) -> &mut Option<RoleChild> {
        match role {
            Role::Device => &mut self.device,
            Role::Core => &mut self.core,
        }
    }
}

impl HostOps for Android {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn sleep(&mut self, duration: Duration) {
        self.shutdown = self.signals.block_on(async {
            tokio::select! {
                _ = self.interrupt.recv() => true,
                _ = self.terminate.recv() => true,
                () = tokio::time::sleep(duration) => false,
            }
        });
    }

    fn shutdown(&self) -> bool {
        self.shutdown
    }

    fn stop_marker(&mut self) -> Result<bool> {
        layout_io::stopped()
    }

    fn acknowledge(&mut self, finished: bool) -> Result<()> {
        let status = launcher::command(if finished { "finish" } else { "resume" })
            .stdin(Stdio::null())
            .status()
            .map_err(|_| "run marker helper")?;
        if status.success() {
            Ok(())
        } else {
            Err("marker helper failed")
        }
    }

    fn candidate(&mut self, slot: Slot) -> Result<Digest> {
        layout_io::digest(slot)
    }

    fn load_selection(&mut self) -> Result<Option<Selection>> {
        self.layout.as_ref().ok_or("supervisor layout unavailable")?.load()
    }

    fn save_selection(&mut self, selection: &Selection) -> Result<()> {
        self.layout
            .as_ref()
            .ok_or("supervisor layout unavailable")?
            .save(selection)
    }

    fn spawn(&mut self, role: Role, slot: Slot, identity: Identity) -> Result<()> {
        *self.child(role) = Some(RoleChild::spawn(role, slot, identity)?);
        Ok(())
    }

    fn observe(&mut self, role: Role) -> Result<ChildStatus> {
        let origin = self.origin;
        self.child(role).as_ref().ok_or("missing role process")?.observe(origin)
    }

    fn stop(&mut self, role: Role, why: Stop) -> Result<String> {
        let child = self.child(role);
        let status = child.as_mut().ok_or("missing role process")?.stop(why)?;
        *child = None;
        Ok(status)
    }

    fn log(&mut self, entry: Log<'_>) {
        let mut log = std::io::stderr().lock();
        let _ = match entry {
            Log::Start(role, slot) => writeln!(log, "echo-bridge boot: start {} {}", role.name(), slot.name()),
            Log::Exit(role, why, status) => {
                writeln!(log, "echo-bridge boot: exit {} {why:?} status={status}", role.name())
            }
            Log::Fallback { current, previous } => {
                writeln!(
                    log,
                    "echo-bridge boot: fallback current={} previous={}",
                    current.as_str(),
                    previous.as_str()
                )
            }
            Log::FirewallReady => writeln!(log, "echo-bridge boot: firewall ready"),
            Log::FirewallUnhealthy(stage) => writeln!(log, "echo-bridge boot: firewall unhealthy: {stage}"),
        };
    }

    fn resolvers(&mut self) -> Result<Vec<IpAddr>> {
        self.firewall.resolvers(&self.signals)
    }

    fn filter(&mut self, family: Family) -> Result<Option<String>> {
        self.firewall.filter(&self.signals, family)
    }

    fn restore_filter(&mut self, family: Family, transaction: &str) -> Result<()> {
        self.firewall.restore(&self.signals, family, transaction)
    }

    fn ipv6_disabled(&mut self, enforce: bool) -> Result<bool> {
        self.firewall.ipv6_disabled(enforce)
    }

    fn usb_adb_only(&mut self, enforce: bool) -> Result<bool> {
        self.firewall.usb_adb_only(&self.signals, enforce)
    }
}
