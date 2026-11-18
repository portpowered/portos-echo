//! Guarded write observer. Production connectors never pass through this socket.
use super::receipt::Context;
use crate::Error;
use echo_contracts::wyoming::{Decoder, encode};
use std::net::SocketAddr;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

pub(super) struct Proxy {
    pub address: SocketAddr,
    task: JoinHandle<()>,
}

impl Proxy {
    pub async fn bind(destination: SocketAddr, context: Context) -> Result<Self, Error> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| Error::Control)?;
        let address = listener.local_addr().map_err(|_| Error::Control)?;
        let task = tokio::spawn(async move {
            if run(listener, destination, &context).await.is_err() {
                context.failed();
            }
        });
        Ok(Self { address, task })
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(listener: TcpListener, destination: SocketAddr, context: &Context) -> Result<(), Error> {
    loop {
        let (mut incoming, _) = listener.accept().await.map_err(|_| Error::Control)?;
        let mut outgoing = TcpStream::connect(destination).await.map_err(|_| Error::Control)?;
        let (mut input, mut input_writer) = incoming.split();
        let (mut output_reader, mut output) = outgoing.split();
        let upstream = async {
            let mut decoder = Decoder::new();
            let mut buffer = [0; 8192];
            loop {
                let size = input.read(&mut buffer).await.map_err(|_| Error::Control)?;
                if size == 0 {
                    return decoder.finish().map_err(|_| Error::Control);
                }
                for frame in decoder.feed(&buffer[..size]).map_err(|_| Error::Control)? {
                    output.write_all(&encode(&frame)).await.map_err(|_| Error::Control)?;
                    if frame.header.r#type == "audio-chunk" {
                        context.wrote(&frame.payload)?;
                    }
                }
            }
        };
        tokio::select! {
            result = upstream => result?,
            result = tokio::io::copy(&mut output_reader, &mut input_writer) => {
                result.map_err(|_| Error::Control)?;
            },
        }
    }
}
