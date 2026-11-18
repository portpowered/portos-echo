use super::{Command, generation};
use crate::wyoming_events;
use crate::{ClientOptions, Error};
use std::{future::Future, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, watch},
    time::{sleep, timeout},
};

struct Backoff {
    next: Duration,
    cap: Duration,
}

impl Backoff {
    fn new(opts: &ClientOptions) -> Self {
        Self {
            next: opts.reconnect_initial,
            cap: opts.reconnect_cap,
        }
    }
    async fn wait(&mut self) {
        sleep(self.next).await;
        self.next = self.next.saturating_mul(2).min(self.cap);
    }
}

pub(crate) async fn connect<T, F, Fut>(opts: &ClientOptions, connector: &mut F, delayed: bool) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::io::Result<T>>,
{
    let mut backoff = Backoff::new(opts);
    if delayed {
        backoff.wait().await;
    }
    loop {
        if let Ok(Ok(socket)) = timeout(opts.connect_timeout, connector()).await {
            return socket;
        }
        backoff.wait().await;
    }
}

pub(crate) struct Supervisor {
    pub(crate) opts: ClientOptions,
    pub(crate) commands: mpsc::Receiver<Command>,
    pub(crate) events: wyoming_events::Sender,
    pub(crate) end: watch::Sender<Option<Error>>,
    pub(crate) reset: watch::Receiver<u64>,
}

impl Supervisor {
    pub(crate) async fn serve<T, F, Fut>(mut self, mut socket: T, mut connector: F)
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnMut() -> Fut,
        Fut: Future<Output = std::io::Result<T>>,
    {
        let mut enabled = false;
        loop {
            let (error, closing) = generation(
                socket,
                self.opts.clone(),
                &mut self.commands,
                self.events.clone(),
                &mut self.reset,
                &mut enabled,
            )
            .await;
            if closing || error == Error::Capacity {
                self.end.send_replace(Some(error));
                break;
            }
            // Never carry old generation commands/audio into another socket.
            while let Ok(command) = self.commands.try_recv() {
                if reject(Some(command)) {
                    self.end.send_replace(Some(Error::Closed));
                    return;
                }
            }
            let reconnect = connect(&self.opts, &mut connector, true);
            tokio::pin!(reconnect);
            socket = loop {
                tokio::select! {
                    socket = &mut reconnect => break socket,
                    command = self.commands.recv() => {
                        if reject(command) { self.end.send_replace(Some(Error::Closed)); return; }
                    }
                }
            };
        }
    }
}

fn reject(command: Option<Command>) -> bool {
    match command {
        Some(Command::Close(reply)) => {
            let _ = reply.send(Ok(()));
            return true;
        }
        Some(Command::Describe(reply)) => {
            let _ = reply.send(Err(Error::Disconnected));
        }
        Some(Command::Play(_, _, reply)) => {
            let _ = reply.send(Err(Error::Disconnected));
        }
        Some(Command::CutWithStats(reply)) => {
            let _ = reply.send(Err(Error::Disconnected));
        }
        #[cfg(test)]
        Some(Command::Cut(reply)) => {
            let _ = reply.send(Err(Error::Disconnected));
        }
        Some(
            Command::Register(reply) | Command::EndRun(reply) | Command::Pause(reply) | Command::Error(_, _, reply),
        ) => {
            let _ = reply.send(Err(Error::Disconnected));
        }
        None => return true,
    }
    false
}

#[cfg(test)]
#[path = "wyoming_reconnect_tests.rs"]
mod wyoming_tests;
