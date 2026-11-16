//! Bounded HTTP parser and sessions; browser origins cannot operate the grant.
use super::bridge_operations::{Operation, Request};
use echo_contracts::secret::Secret;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc, oneshot},
    task::JoinSet,
};

pub(super) async fn serve(listener: TcpListener, token: Secret<String>, send: mpsc::Sender<Request>) {
    let token = Arc::new(token);
    let slot = Arc::new(Semaphore::new(1));
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            result = listener.accept(), if sessions.len() < 16 => {
                let Ok((stream, _)) = result else { break };
                sessions.spawn(session(stream, token.clone(), send.clone(), slot.clone()));
            }
            _ = sessions.join_next(), if !sessions.is_empty() => (),
        }
    }
}

async fn session(mut stream: TcpStream, token: Arc<Secret<String>>, send: mpsc::Sender<Request>, slot: Arc<Semaphore>) {
    let read = tokio::time::timeout(Duration::from_secs(1), read(&mut stream, token.expose())).await;
    let operation = match read {
        Ok(Ok(operation)) => operation,
        Ok(Err(code)) => {
            let _ = respond(&mut stream, code, Value::Null).await;
            return;
        }
        Err(_) => return,
    };
    let Ok(permit) = slot.try_acquire_owned() else {
        let _ = respond(&mut stream, 409, Value::Null).await;
        return;
    };
    let (reply, receive) = oneshot::channel();
    if send
        .try_send(Request {
            operation,
            reply,
            permit,
        })
        .is_err()
    {
        return;
    }
    let mut byte = [0];
    let result = tokio::select! {
        biased;
        _ = stream.read(&mut byte) => return,
        result = receive => result,
    };
    let (code, body) = match result {
        Ok(Ok(outcome)) => (200, json!({"outcome":outcome})),
        Ok(Err(error)) => (422, json!({"error":super::category(&error)})),
        Err(_) => return,
    };
    let _ = tokio::time::timeout(Duration::from_secs(1), respond(&mut stream, code, body)).await;
}

async fn read(stream: &mut TcpStream, token: &str) -> Result<Operation, u16> {
    let mut bytes = [0; 4096];
    let mut used = 0;
    let end = loop {
        if used == bytes.len() {
            return Err(400);
        }
        let count = stream.read(&mut bytes[used..]).await.map_err(|_| 400_u16)?;
        if count == 0 {
            return Err(400);
        }
        used += count;
        if let Some(end) = bytes[..used].windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| 400_u16)?;
    let length = headers(text, token)?;
    if length > bytes.len() - end || used > end + length {
        return Err(400);
    }
    stream
        .read_exact(&mut bytes[used..end + length])
        .await
        .map_err(|_| 400_u16)?;
    operation(&bytes[end..end + length])
}

fn headers(text: &str, token: &str) -> Result<usize, u16> {
    let mut lines = text.split("\r\n");
    let words: Vec<_> = lines.next().unwrap_or_default().split(' ').collect();
    if words.len() != 3 || words[2] != "HTTP/1.1" {
        return Err(400);
    }
    if words[1] != "/operator/mcp" {
        return Err(404);
    }
    if words[0] != "POST" {
        return Err(405);
    }
    let mut authorization = None;
    let mut length = None;
    let mut json_body = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(400_u16)?;
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "origin" => return Err(403),
            "authorization" if authorization.replace(value).is_some() => return Err(401),
            "content-length" if length.replace(value.parse::<usize>().map_err(|_| 400_u16)?).is_some() => {
                return Err(400);
            }
            "transfer-encoding" => return Err(400),
            "content-type" => json_body = value == "application/json",
            _ => (),
        }
    }
    if authorization.and_then(|a| a.strip_prefix("Bearer ")) != Some(token) {
        return Err(401);
    }
    if !json_body {
        return Err(400);
    }
    length.ok_or(400)
}

fn operation(bytes: &[u8]) -> Result<Operation, u16> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| 400_u16)?;
    let object = value.as_object().filter(|o| o.len() == 1).ok_or(400_u16)?;
    Operation::parse(object.get("operation").and_then(Value::as_str).ok_or(400_u16)?).ok_or(400)
}

async fn respond(stream: &mut TcpStream, code: u16, value: Value) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        _ => "Bad Request",
    };
    let body = if value.is_null() {
        String::new()
    } else {
        value.to_string()
    };
    let allow = if code == 405 { "Allow: POST\r\n" } else { "" };
    stream.write_all(format!("HTTP/1.1 {code} {reason}\r\n{allow}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_link_control_offline_payload_and_header_validation() {
        for body in [
            "{}",
            r#"{"operation":"signin"}"#,
            r#"{"operation":"link","extra":1}"#,
            r#"{"operation":0}"#,
        ] {
            assert_eq!(operation(body.as_bytes()), Err(400));
        }
        assert_eq!(operation(br#"{"operation":"refresh"}"#), Ok(Operation::Refresh));
        let valid = "POST /operator/mcp HTTP/1.1\r\nAuthorization: Bearer synthetic-owner\r\nContent-Type: application/json\r\nContent-Length: 20\r\n\r\n";
        assert_eq!(headers(valid, "synthetic-owner"), Ok(20));
        assert_eq!(headers(valid, "wrong"), Err(401));
        assert_eq!(
            headers(
                &valid.replace("Content-Type:", "Origin: local\r\nContent-Type:"),
                "synthetic-owner"
            ),
            Err(403)
        );
        assert_eq!(
            headers(
                &valid.replace("Content-Type:", "Content-Length: 20\r\nContent-Type:"),
                "synthetic-owner"
            ),
            Err(400)
        );
    }
}
