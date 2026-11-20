//! A detached owner finishes allocation and cleanup even when its caller cancels.
use super::{Health, ScreenEvents};
use crate::{Failure, Result, adb::Adb, cli::Options};
use echo_contracts::secret::Secret;
use std::ffi::OsString;
use tokio::{sync::oneshot, task::JoinHandle, time::Instant};

/// Only dynamically allocated forwards belong to this lease. No remove-all,
/// fixed local port, device serial or pairing credential is exposed by it.
pub struct ForwardSession {
    screen: u16,
    health: u16,
    release: Option<oneshot::Sender<()>>,
    owner: JoinHandle<Result<()>>,
}

impl ForwardSession {
    /// Remote ports come from the installed configuration, never a host address.
    /// # Errors
    /// Fixed device, allocation or deadline categories.
    pub async fn open(options: &Options, remote: [u16; 2], deadline: Instant) -> Result<Self> {
        if remote.contains(&0) {
            return Err(Failure("setup_forward_failed"));
        }
        if Instant::now() >= deadline {
            return Err(Failure("setup_timeout"));
        }
        let mut adb = Adb::discover(options.adb.as_deref())?;
        tokio::time::timeout_at(deadline, adb.select(options.serial.as_deref()))
            .await
            .map_err(|_| Failure("setup_timeout"))??;
        Self::selected(adb, remote, deadline).await
    }

    pub(super) async fn selected(adb: Adb, remote: [u16; 2], deadline: Instant) -> Result<Self> {
        if remote.contains(&0) {
            return Err(Failure("setup_forward_failed"));
        }
        if Instant::now() >= deadline {
            return Err(Failure("setup_timeout"));
        }
        let (ready, response) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let owner = tokio::spawn(own(adb, remote, ready, released, deadline));
        let outcome = tokio::time::timeout_at(deadline, response)
            .await
            .map_err(|_| Failure("setup_timeout"))
            .and_then(|result| result.map_err(|_| Failure("setup_forward_failed")))
            .and_then(std::convert::identity);
        let ports = match outcome {
            Ok(ports) => ports,
            Err(error) => {
                // Stop requesting work, then drain its bounded cleanup before
                // a failure can reach a command's terminal result/runtime exit.
                drop(release);
                owner.await.map_err(|_| Failure("setup_forward_cleanup_failed"))??;
                return Err(error);
            }
        };
        Ok(Self {
            screen: ports[0],
            health: ports[1],
            release: Some(release),
            owner,
        })
    }

    /// # Errors
    /// The existing bounded HTTP observer's fixed categories.
    pub async fn health(&self, deadline: Instant) -> Result<Health> {
        super::health(self.health, deadline).await
    }

    /// # Errors
    /// The existing authenticated SSE observer's fixed categories.
    pub async fn screen(&self, pairing: &Secret<String>, deadline: Instant) -> Result<ScreenEvents> {
        ScreenEvents::connect(self.screen, pairing, deadline).await
    }

    /// Observe existing grant status on this lease's authenticated screen port.
    /// # Errors
    /// Fixed settings, pairing, HTTP or deadline categories.
    pub async fn link_required(&self, pairing: &Secret<String>, deadline: Instant) -> Result<bool> {
        super::link_required(self.screen, pairing, deadline).await
    }

    /// Wait for cleanup before reporting an operation's result. Dropping this
    /// session also releases the lease, with cleanup continuing on the runtime.
    /// # Errors
    /// Fixed cleanup failure, never adb bytes or identifiers.
    pub async fn close(mut self) -> Result<()> {
        self.release.take();
        self.owner.await.map_err(|_| Failure("setup_forward_cleanup_failed"))?
    }
}

async fn own(
    adb: Adb,
    remote: [u16; 2],
    ready: oneshot::Sender<Result<[u16; 2]>>,
    released: oneshot::Receiver<()>,
    deadline: Instant,
) -> Result<()> {
    let mut allocated = Vec::new();
    let result = allocate(&adb, remote, &mut allocated, &ready, deadline).await;
    let wait = result.is_ok();
    if ready.send(result).is_ok() && wait {
        let _ = released.await;
    }
    let mut clean = true;
    for (port, remote) in allocated.into_iter().zip(remote).rev() {
        // Removal is scoped to the selected device and this allocation only.
        clean &= adb.remove_forward(port, remote).await.is_ok();
    }
    if !clean {
        return Err(Failure("setup_forward_cleanup_failed"));
    }
    Ok(())
}

async fn allocate(
    adb: &Adb,
    remote: [u16; 2],
    allocated: &mut Vec<u16>,
    ready: &oneshot::Sender<Result<[u16; 2]>>,
    deadline: Instant,
) -> Result<[u16; 2]> {
    for remote in remote {
        if ready.is_closed() || Instant::now() >= deadline {
            return Err(Failure("setup_timeout"));
        }
        let raw = adb
            .forward(&[
                OsString::from("--no-rebind"),
                OsString::from("tcp:0"),
                OsString::from(format!("tcp:{remote}")),
            ])
            .await?;
        let port = parse_port(&raw)?;
        if allocated.contains(&port) {
            return Err(Failure("setup_forward_failed"));
        }
        allocated.push(port);
    }
    Ok([allocated[0], allocated[1]])
}

fn parse_port(raw: &str) -> Result<u16> {
    let number = raw.strip_suffix('\n').unwrap_or(raw);
    if number.is_empty() || number.starts_with('0') || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Failure("setup_forward_failed"));
    }
    number
        .parse()
        .ok()
        .filter(|port| *port != 0)
        .ok_or(Failure("setup_forward_failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocated_port_is_one_canonical_nonzero_number() {
        assert_eq!(parse_port("12345\n"), Ok(12345));
        assert_eq!(parse_port("65535"), Ok(65535));
        for raw in [
            "",
            "0",
            "01",
            "65536",
            "-1",
            " 123",
            "123\n\n",
            "123\nSYNTHETIC-TOKEN",
            "123\r\n",
        ] {
            assert_eq!(parse_port(raw), Err(Failure("setup_forward_failed")));
        }
    }
}
