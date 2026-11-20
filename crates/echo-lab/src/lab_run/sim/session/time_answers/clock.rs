//! Loopback SNTP gives the real bridge a controlled call-start wall clock.
use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{net::UdpSocket, task::JoinHandle};

// 2026-02-03 03:30:05 UTC, expressed as NTP seconds.
const INITIAL: u64 = 3_979_078_205;

pub(super) struct Clock {
    seconds: Arc<AtomicU64>,
    task: JoinHandle<()>,
}

impl Clock {
    pub async fn start() -> Result<(Self, SocketAddr), String> {
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .map_err(|_| "time answers: SNTP bind failed")?;
        let address = socket
            .local_addr()
            .map_err(|_| "time answers: SNTP address unavailable")?;
        let seconds = Arc::new(AtomicU64::new(INITIAL));
        let task = tokio::spawn(serve(socket, seconds.clone()));
        Ok((Self { seconds, task }, address))
    }

    pub fn select_later_sample(&self) {
        self.seconds.store(INITIAL + 420, Ordering::Relaxed);
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(socket: UdpSocket, seconds: Arc<AtomicU64>) {
    let mut request = [0; 48];
    while let Ok((48, address)) = socket.recv_from(&mut request).await {
        let timestamp = (seconds.load(Ordering::Relaxed) << 32).to_be_bytes();
        let mut response = [0; 48];
        response[0] = 0x24; // Version 4, server mode.
        response[1] = 1; // Primary reference.
        response[24..32].copy_from_slice(&request[40..48]);
        response[32..40].copy_from_slice(&timestamp);
        response[40..48].copy_from_slice(&timestamp);
        if socket.send_to(&response, address).await.is_err() {
            break;
        }
    }
}
