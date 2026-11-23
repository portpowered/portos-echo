//! Bounded HTTP parser and sessions; browser origins cannot operate the grant.
use super::bridge_operations::{Operation, Operator, StartError};
use crate::bridge_settings::openai_account::Account;
use echo_contracts::secret::Secret;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinSet,
};

#[derive(Debug, PartialEq, Eq)]
enum Request {
    PortOs(Operation),
    OpenAiStart,
    OpenAiStatus,
}

pub(super) async fn serve(listener: TcpListener, token: Secret<String>, operator: Operator, account: Option<Account>) {
    let token = Arc::new(token);
    let mut sessions = JoinSet::new();
    loop {
        tokio::select! {
            result = listener.accept(), if sessions.len() < 16 => {
                let Ok((stream, _)) = result else { break };
                sessions.spawn(session(stream, token.clone(), operator.clone(), account.clone()));
            }
            _ = sessions.join_next(), if !sessions.is_empty() => (),
        }
    }
}

async fn session(mut stream: TcpStream, token: Arc<Secret<String>>, operator: Operator, account: Option<Account>) {
    let read = tokio::time::timeout(Duration::from_secs(1), read(&mut stream, token.expose())).await;
    let (operation, streaming) = match read {
        Ok(Ok(operation)) => operation,
        Ok(Err(code)) => {
            let _ = respond(&mut stream, code, Value::Null, false).await;
            return;
        }
        Err(_) => return,
    };
    let Request::PortOs(operation) = operation else {
        let (code, body) = match operation {
            Request::OpenAiStatus => (
                200,
                account.map_or_else(|| json!({"state":"unavailable"}), |account| account.recovery_status()),
            ),
            _ if account.is_some_and(|account| account.relink_start()) => (202, json!({"outcome":"accepted"})),
            _ => (409, Value::Null),
        };
        let _ = tokio::time::timeout(Duration::from_secs(1), respond(&mut stream, code, body, true)).await;
        return;
    };
    let receipt = match operator.start(operation) {
        Ok(receipt) => receipt,
        Err(StartError::Busy) => {
            let _ = respond(&mut stream, 409, Value::Null, false).await;
            return;
        }
        Err(StartError::Closed) => return,
    };
    let result = completion(&mut stream, receipt.result, receipt.progress, streaming).await;
    let (code, body) = match result {
        Ok(Ok(outcome)) => (200, json!({"outcome":outcome})),
        Ok(Err(error)) => (422, json!({"error":super::category(&error)})),
        Err(()) => return,
    };
    if streaming {
        let _ = event(&mut stream, body).await;
    } else {
        let _ = tokio::time::timeout(Duration::from_secs(1), respond(&mut stream, code, body, false)).await;
    }
}

async fn completion(
    stream: &mut TcpStream,
    mut receive: oneshot::Receiver<Result<&'static str, echo_mcp_auth::AuthError>>,
    mut updates: mpsc::Receiver<echo_screen_contract::UiState>,
    streaming: bool,
) -> Result<Result<&'static str, echo_mcp_auth::AuthError>, ()> {
    if streaming {
        tokio::time::timeout(Duration::from_secs(1), stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        )).await.map_err(|_| ())?.map_err(|_| ())?;
    }
    let mut byte = [0];
    loop {
        tokio::select! {
            biased;
            _ = stream.read(&mut byte) => return Err(()),
            state = updates.recv(), if !updates.is_closed() || !updates.is_empty() => {
                if streaming && let Some(state) = state {
                    event(stream, json!({"state":state})).await?;
                }
            }
            result = &mut receive => return result.map_err(|_| ()),
        }
    }
}

async fn event(stream: &mut TcpStream, value: Value) -> Result<(), ()> {
    tokio::time::timeout(
        Duration::from_secs(1),
        stream.write_all(format!("data: {value}\n\n").as_bytes()),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}

async fn read(stream: &mut TcpStream, token: &str) -> Result<(Request, bool), u16> {
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
    let streaming = text
        .split("\r\n")
        .any(|line| line.eq_ignore_ascii_case("Accept: text/event-stream"));
    if length > bytes.len() - end || used > end + length {
        return Err(400);
    }
    stream
        .read_exact(&mut bytes[used..end + length])
        .await
        .map_err(|_| 400_u16)?;
    operation(&bytes[end..end + length]).map(|operation| (operation, streaming))
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

fn operation(bytes: &[u8]) -> Result<Request, u16> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| 400_u16)?;
    let object = value.as_object().filter(|o| o.len() == 1).ok_or(400_u16)?;
    match object.get("operation").and_then(Value::as_str).ok_or(400_u16)? {
        "openai_signin_start" => Ok(Request::OpenAiStart),
        "openai_signin_status" => Ok(Request::OpenAiStatus),
        operation => Operation::parse(operation).map(Request::PortOs).ok_or(400),
    }
}

async fn respond(stream: &mut TcpStream, code: u16, value: Value, no_store: bool) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        202 => "Accepted",
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
    let cache = if no_store { "Cache-Control: no-store\r\n" } else { "" };
    stream.write_all(format!("HTTP/1.1 {code} {reason}\r\n{allow}{cache}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
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
        assert_eq!(
            operation(br#"{"operation":"refresh"}"#),
            Ok(Request::PortOs(Operation::Refresh))
        );
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
