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
pub(crate) async fn run(listener: TcpListener, app: Router, stop: watch::Receiver<bool>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            () = crate::cancelled(stop.clone()) => break,
            Some(_) = connections.join_next() => {},
            accepted = listener.accept() => {
                let Ok((socket, _)) = accepted else { break; };
                connections.spawn(connection(socket, app.clone(), stop.clone()));
            }
        }
    }
    drop(listener);
    connections.shutdown().await;
}

async fn connection(socket: TcpStream, app: Router, stop: watch::Receiver<bool>) {
    let (cancel, cancel_rx) = watch::channel(false);
    let app = app.layer(Extension(cancel));
    let service = TowerToHyperService::new(app);
    tokio::select! {
        biased;
        () = crate::cancelled(stop) => {},
        () = crate::cancelled(cancel_rx) => {},
        _ = http1::Builder::new().serve_connection(TokioIo::new(socket), service) => {},
    }
}
