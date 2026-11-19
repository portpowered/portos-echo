//! One signal-driven transport outage before wake, through the native host.
use super::{Endpoint, Relay, Timing};
use std::net::SocketAddr;
use tokio::{
    net::TcpListener,
    sync::{oneshot, watch},
};

pub(super) async fn serve(
    listener: TcpListener,
    host: SocketAddr,
    mut endpoint: Endpoint,
    observed: &watch::Sender<Timing>,
    restart: Option<oneshot::Receiver<()>>,
) -> Result<(), String> {
    if let Some(restart) = restart {
        tokio::select! {
            result = super::serve(&listener, host, &mut endpoint, observed) => {
                result?;
                return Err("wire: first connection ended before requested reconnect".into());
            }
            result = restart => result.map_err(|_| "wire: reconnect cancelled")?,
        }
    }
    super::serve(&listener, host, &mut endpoint, observed).await
}

impl Relay {
    pub fn reconnect(&mut self) -> Result<(), String> {
        self.restart
            .take()
            .ok_or("wire: reconnect not configured")?
            .send(())
            .map_err(|()| "wire: reconnect observer stopped".into())
    }

    pub fn reconnected(&self) -> Result<bool, String> {
        let timing = self.timing();
        if let Some(error) = timing.failure {
            return Err(error);
        }
        Ok(timing.session.registrations_ready())
    }
}
