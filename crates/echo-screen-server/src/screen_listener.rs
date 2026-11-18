use axum::{Extension, Router};
use hyper::server::conn::http1;
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinSet,
};

/// The listener owns every connection task. Cancellation drops the socket even
/// when the HTTP writer is blocked by a client that no longer reads.
pub(crate) async fn run(
    listener: TcpListener,
    app: Router,
    stop: watch::Receiver<bool>,
    drain: watch::Receiver<Option<tokio::time::Instant>>,
) -> usize {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            () = crate::cancelled(stop.clone()) => break,
            () = crate::draining(drain.clone()) => break,
            Some(_) = connections.join_next() => {},
            accepted = listener.accept() => {
                let Ok((socket, _)) = accepted else { break; };
                if connections.len() < 128 {
                    connections.spawn(connection(socket, app.clone(), stop.clone(), drain.clone()));
                }
            }
        }
    }
    drop(listener);
    let deadline = *drain.borrow();
    finish(connections, stop, deadline).await
}

async fn finish(
    mut connections: JoinSet<()>,
    stop: watch::Receiver<bool>,
    deadline: Option<tokio::time::Instant>,
) -> usize {
    let mut dropped = 0;
    if let Some(deadline) = deadline {
        tokio::select! {
            biased;
            () = crate::cancelled(stop) => {},
            () = async { while connections.join_next().await.is_some() {} } => {},
            () = tokio::time::sleep_until(deadline) => { dropped = connections.len(); },
        }
    }
    connections.shutdown().await;
    dropped
}

async fn connection(
    socket: TcpStream,
    app: Router,
    stop: watch::Receiver<bool>,
    drain: watch::Receiver<Option<tokio::time::Instant>>,
) {
    let (cancel, cancel_rx) = watch::channel(false);
    let app = app.layer(Extension(cancel));
    let service = TowerToHyperService::new(app);
    let http = http1::Builder::new().serve_connection(TokioIo::new(socket), service);
    tokio::pin!(http);
    tokio::select! {
        biased;
        () = crate::cancelled(stop.clone()) => return,
        () = crate::cancelled(cancel_rx.clone()) => return,
        _ = &mut http => return,
        () = crate::draining(drain.clone()) => {},
    }
    http.as_mut().graceful_shutdown();
    tokio::select! {
        biased;
        () = crate::cancelled(stop) => {},
        () = crate::cancelled(cancel_rx) => {},
        _ = http => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, task::Poll, time::Duration};
    use tokio::time::{Instant, advance};

    #[tokio::test(start_paused = true)]
    async fn stalled_connections_share_one_deadline_and_cancellation_skips_it() {
        for cancel in [false, true] {
            let mut connections = JoinSet::new();
            let mut receipts = Vec::new();
            for _ in 0..3 {
                let (send, receive) = tokio::sync::oneshot::channel::<()>();
                receipts.push(receive);
                connections.spawn(async move {
                    let _held = send;
                    std::future::pending::<()>().await;
                });
            }
            let (stop, stop_rx) = watch::channel(false);
            let started = Instant::now();
            let closing = finish(connections, stop_rx, Some(started + Duration::from_millis(250)));
            tokio::pin!(closing);
            assert!(std::future::poll_fn(|cx| Poll::Ready(closing.as_mut().poll(cx).is_pending())).await);
            if cancel {
                stop.send_replace(true);
            } else {
                advance(Duration::from_millis(249)).await;
                assert!(std::future::poll_fn(|cx| Poll::Ready(closing.as_mut().poll(cx).is_pending())).await);
                advance(Duration::from_millis(1)).await;
            }
            assert_eq!(closing.await, if cancel { 0 } else { 3 });
            assert_eq!(
                Instant::now() - started,
                Duration::from_millis(if cancel { 0 } else { 250 })
            );
            for receipt in receipts {
                assert!(receipt.await.is_err());
            }
        }
    }
}
