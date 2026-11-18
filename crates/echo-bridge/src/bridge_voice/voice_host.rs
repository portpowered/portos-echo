//! Staged cancellation keeps device cleanup inside the authenticated IO lease.
use crate::{Error, bridge_codex::with_host_options};
use echo_codex::codex_io::RpcIo;
use echo_link::{EchoLink, LinkConnector};
use std::{cell::Cell, future::Future, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    process::{ChildStdin, ChildStdout},
    sync::watch,
    time::{Instant, timeout, timeout_at},
};

pub use crate::bridge_codex::HostOptions;

/// Initialize and authenticate one child before connecting the device. Capture
/// and probe must succeed before invoking the session; Connected still requires
/// the session's successful media negotiation. No legacy call path is invoked.
///
/// Before admission, cancellation interrupts startup. Afterwards it reaches the
/// callback through a watch receiver, so the conversation can await call/media
/// stop before returning. The callback must service that receiver and clean up
/// its calls on every return. This owner then closes the device and reaps the
/// child, retaining the private home lease throughout both operations.
///
/// # Errors
/// Fixed process/account/device/deadline categories. No session on unsuccessful
/// startup. Failed capture/probe and session errors still await device close.
pub async fn with_device<C: LinkConnector, T>(
    options: HostOptions<'_>,
    connector: C,
    cancelled: impl Future<Output = ()>,
    session: impl AsyncFnOnce(&mut RpcIo<ChildStdout, ChildStdin>, &mut C::Link, watch::Receiver<bool>) -> Result<T, Error>,
) -> Result<T, Error> {
    let admitted = Cell::new(false);
    let (stop, stopping) = watch::channel(false);
    let startup_stop = stopping.clone();
    let deadline = options.deadline;
    let operation = with_host_options(
        options,
        async {
            stopped(startup_stop).await;
            if admitted.get() {
                std::future::pending::<()>().await;
            }
        },
        async |io| {
            admitted.set(true);
            device(io, &connector, deadline, stopping, session).await
        },
    );
    tokio::pin!(operation, cancelled);
    tokio::select! {
        biased;
        () = cancelled => {
            stop.send_replace(true);
            operation.await
        }
        result = &mut operation => result,
    }
}

async fn device<C: LinkConnector, T, R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    io: &mut RpcIo<R, W>,
    connector: &C,
    deadline: Instant,
    stopping: watch::Receiver<bool>,
    session: impl AsyncFnOnce(&mut RpcIo<R, W>, &mut C::Link, watch::Receiver<bool>) -> Result<T, Error>,
) -> Result<T, Error> {
    if *stopping.borrow() {
        return Err(Error::Control);
    }
    if Instant::now() >= deadline {
        return Err(Error::Deadline);
    }
    let mut client = tokio::select! {
        biased;
        () = stopped(stopping.clone()) => return Err(Error::Control),
        result = timeout_at(deadline, connector.connect()) => result.map_err(|_| Error::Deadline)??,
    };
    let startup = async {
        client.enable_capture(30).await?;
        client.probe().await.map_err(Error::from)
    };
    let ready = tokio::select! {
        biased;
        () = stopped(stopping.clone()) => Err(Error::Control),
        result = timeout_at(deadline, startup) => result.unwrap_or(Err(Error::Deadline)),
    };
    let result = match ready {
        Ok(()) if Instant::now() >= deadline => Err(Error::Deadline),
        Ok(()) => session(io, &mut client, stopping).await,
        Err(error) => Err(error),
    };
    let cleanup = timeout(Duration::from_secs(2), client.close())
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|result| result.map_err(Error::from));
    // Preserve the operational cause while still awaiting all cleanup.
    result.and_then(|value| cleanup.map(|()| value))
}

async fn stopped(mut stopping: watch::Receiver<bool>) {
    loop {
        if *stopping.borrow_and_update() {
            return;
        }
        if stopping.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_link::fake::{FakeConnector, Observer};

    #[tokio::test(start_paused = true)]
    async fn expired_or_cancelled_device_admission_never_connects() {
        let (stream, _server) = tokio::io::duplex(1024);
        let (read, write) = tokio::io::split(stream);
        let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
        for cancelled in [false, true] {
            let observer = Observer::default();
            let (_stop, stopping) = watch::channel(cancelled);
            let result = device(
                &mut io,
                &FakeConnector::new(observer.clone()),
                Instant::now(),
                stopping,
                async |_, _, _| -> Result<(), Error> { unreachable!() },
            )
            .await;
            assert_eq!(result, Err(if cancelled { Error::Control } else { Error::Deadline }));
            assert_eq!(observer.commands(), []);
        }
    }

    struct BlockedConnector(tokio::sync::Notify);

    impl LinkConnector for BlockedConnector {
        type Link = echo_link::fake::FakeLink;

        async fn connect(&self) -> Result<Self::Link, echo_link::Error> {
            self.0.notify_one();
            std::future::pending().await
        }
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_connect_observes_cancellation_and_exact_absolute_deadline() {
        for cancelled in [false, true] {
            let (stream, _server) = tokio::io::duplex(1024);
            let (read, write) = tokio::io::split(stream);
            let mut io = RpcIo::new(read, write, echo_codex::codex_rpc::Limits::default()).unwrap();
            let connector = BlockedConnector(tokio::sync::Notify::new());
            let (stop, stopping) = watch::channel(false);
            let start = Instant::now();
            let operation = device(
                &mut io,
                &connector,
                start + Duration::from_secs(5),
                stopping,
                async |_, _, _| -> Result<(), Error> { unreachable!() },
            );
            let signal = async {
                connector.0.notified().await;
                if cancelled {
                    stop.send_replace(true);
                } else {
                    tokio::time::advance(Duration::from_secs(5)).await;
                }
            };
            let (result, ()) = tokio::join!(operation, signal);
            assert_eq!(result, Err(if cancelled { Error::Control } else { Error::Deadline }));
            assert_eq!(
                start.elapsed(),
                if cancelled {
                    Duration::ZERO
                } else {
                    Duration::from_secs(5)
                }
            );
        }
    }
}
