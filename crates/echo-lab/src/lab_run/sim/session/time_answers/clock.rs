//! Loopback SNTP gives the real bridge a controlled call-start wall clock.
use std::net::SocketAddr;
use tokio::{net::UdpSocket, task::JoinHandle};

pub(super) struct Clock(JoinHandle<()>);

impl Clock {
    pub async fn start() -> Result<(Self, SocketAddr), String> {
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .map_err(|_| "time answers: SNTP bind failed")?;
        let address = socket
            .local_addr()
            .map_err(|_| "time answers: SNTP address unavailable")?;
        Ok((Self(tokio::spawn(serve(socket))), address))
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn serve(socket: UdpSocket) {
    // 2026-02-03 03:30:05 UTC, expressed as a 32.32 NTP timestamp.
    let timestamp = (3_979_078_205_u64 << 32).to_be_bytes();
    let mut request = [0; 48];
    while let Ok((48, address)) = socket.recv_from(&mut request).await {
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
