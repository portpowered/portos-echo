//! Bounded HTTP/1.1 parser for the local fake, not a production server.
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::call::CallError;

/// Contains synthetic authorization and intentionally has no formatting traits.
#[derive(Clone)]
pub struct ObservedRequest {
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: serde_json::Value,
}

impl ObservedRequest {
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Compare a synthetic header privately without returning its value.
    #[must_use]
    pub fn header_matches(&self, name: &str, expected: &str) -> bool {
        self.headers
            .iter()
            .any(|(key, value)| key.eq_ignore_ascii_case(name) && value == expected)
    }

    #[must_use]
    pub fn has_header(&self, name: &str) -> bool {
        self.headers.iter().any(|(key, _)| key.eq_ignore_ascii_case(name))
    }

    #[must_use]
    pub fn session(&self) -> &serde_json::Value {
        &self.body["session"]
    }
}

pub(crate) async fn request(stream: &mut TcpStream) -> Result<ObservedRequest, CallError> {
    let mut bytes = Vec::new();
    let end = loop {
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
        read(stream, &mut bytes).await?;
    };
    let head = std::str::from_utf8(&bytes[..end]).map_err(|_| CallError::Signaling)?;
    let mut lines = head.split("\r\n");
    let start: Vec<_> = lines.next().ok_or(CallError::Signaling)?.split_whitespace().collect();
    if start.len() != 3 || start[0] != "POST" || start[2] != "HTTP/1.1" {
        return Err(CallError::Signaling);
    }
    let path = start[1].to_owned();
    let headers: Vec<_> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(key, _)| key == "content-length")
        .ok_or(CallError::Signaling)?
        .1
        .parse::<usize>()
        .map_err(|_| CallError::Signaling)?;
    if length > 131_072 {
        return Err(CallError::Signaling);
    }
    while bytes.len() < end + length {
        read(stream, &mut bytes).await?;
    }
    let body = serde_json::from_slice(&bytes[end..end + length]).map_err(|_| CallError::Decode)?;
    Ok(ObservedRequest { path, headers, body })
}

async fn read(stream: &mut TcpStream, bytes: &mut Vec<u8>) -> Result<(), CallError> {
    let mut chunk = [0; 4096];
    let length = stream.read(&mut chunk).await.map_err(|_| CallError::Signaling)?;
    if length == 0 || bytes.len() + length > 147_456 {
        return Err(CallError::Signaling);
    }
    bytes.extend_from_slice(&chunk[..length]);
    Ok(())
}

pub(crate) async fn respond(
    stream: &mut TcpStream,
    status: u16,
    body: &str,
    location: Option<&str>,
) -> Result<(), CallError> {
    let redirect = location.map_or_else(String::new, |url| format!("Location: {url}\r\n"));
    let head = format!(
        "HTTP/1.1 {status} Synthetic\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\nConnection: close\r\n{redirect}\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|_| CallError::Signaling)?;
    stream
        .write_all(body.as_bytes())
        .await
        .map_err(|_| CallError::Signaling)?;
    stream.shutdown().await.map_err(|_| CallError::Signaling)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    // T3: exercise the fake's bounded parser through a real, isolated loopback socket.
    #[tokio::test(start_paused = true)]
    async fn t3_call_fake_rejects_invalid_http_without_recording_a_request() {
        for (request, expected) in [
            ("GET / HTTP/1.1\r\n\r\n", CallError::Signaling),
            ("POST / HTTP/1.1\r\n\r\n", CallError::Signaling),
            ("POST / HTTP/1.1\r\nContent-Length: bad\r\n\r\n", CallError::Signaling),
            (
                "POST / HTTP/1.1\r\nContent-Length: 131073\r\n\r\n",
                CallError::Signaling,
            ),
            ("POST / HTTP/1.1\r\nContent-Length: 2\r\n\r\n{", CallError::Signaling),
            ("POST / HTTP/1.1\r\nContent-Length: 1\r\n\r\nx", CallError::Decode),
            ("POST / HTTP/1.1\r\n", CallError::Signaling),
        ] {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let client = async {
                let mut stream = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
                stream.write_all(request.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            };
            let server = async {
                let (mut stream, _) = listener.accept().await.unwrap();
                assert_eq!(super::request(&mut stream).await.err(), Some(expected));
            };
            tokio::join!(client, server);
        }
    }
}
