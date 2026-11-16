//! One bounded, read-only request at a time; no task per connection.
use crate::Error;
use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
};

#[derive(Clone, Copy)]
pub struct Links {
    pub wyoming: &'static str,
    pub live: &'static str,
    pub relink_required: bool,
    pub kill_switch: bool,
}

impl Default for Links {
    fn default() -> Self {
        Self {
            wyoming: "connecting",
            live: "idle",
            relink_required: false,
            kill_switch: false,
        }
    }
}

pub struct Health {
    pub address: SocketAddr,
    pub state: watch::Sender<Links>,
    task: JoinHandle<()>,
}

impl Health {
    /// # Errors
    /// Only literal IPv4 loopback is admitted; bind failures prevent readiness.
    pub async fn bind(address: SocketAddr) -> Result<Self, Error> {
        if address.ip() != Ipv4Addr::LOCALHOST {
            return Err(Error::Input);
        }
        let listener = TcpListener::bind(address).await.map_err(|_| Error::Health)?;
        let address = listener.local_addr().map_err(|_| Error::Health)?;
        let (state, receive) = watch::channel(Links::default());
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let _ = tokio::time::timeout(Duration::from_secs(1), serve(&mut stream, &receive)).await;
            }
        });
        Ok(Self { address, state, task })
    }

    pub async fn close(self) {
        self.task.abort();
        // Drop also aborts; awaiting by mutable reference retains the owner.
        let mut owner = self;
        let _ = (&mut owner.task).await;
    }
}

impl Drop for Health {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(stream: &mut TcpStream, links: &watch::Receiver<Links>) -> std::io::Result<()> {
    let mut buffer = [0; 4096];
    let mut used = 0;
    loop {
        let count = stream.read(&mut buffer[used..]).await?;
        if count == 0 {
            return Ok(());
        }
        used += count;
        if buffer[..used].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if used == buffer.len() {
            return Ok(());
        }
    }
    let state = *links.borrow();
    let response = response(&buffer[..used], state);
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

#[must_use]
pub fn response(request: &[u8], links: Links) -> String {
    let line = request.split(|&b| b == b'\r').next().unwrap_or_default();
    let Ok(line) = std::str::from_utf8(line) else {
        return String::new();
    };
    let words: Vec<_> = line.split(' ').collect();
    if words.len() != 3 || words[2] != "HTTP/1.1" {
        return String::new();
    }
    let (status, allow, body) = if words[1] != "/healthz" {
        ("404 Not Found", "", String::new())
    } else if words[0] != "GET" {
        ("405 Method Not Allowed", "Allow: GET\r\n", String::new())
    } else {
        (
            "200 OK",
            "",
            serde_json::json!({"status":"running", "wyoming":links.wyoming,"live":links.live,
                "relink_required":links.relink_required,"kill_switch":links.kill_switch})
            .to_string(),
        )
    };
    format!(
        "HTTP/1.1 {status}\r\n{allow}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
