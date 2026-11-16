use crate::AuthError;
use echo_contracts::secret::Secret;
use std::collections::BTreeMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

fn code(target: &str, state: &str) -> Result<Secret<String>, AuthError> {
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}")).map_err(|_| AuthError::InvalidCredential)?;
    let mut fields = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if fields.insert(key.into_owned(), value.into_owned()).is_some() {
            return Err(AuthError::InvalidCredential);
        }
    }
    if fields.get("state").map(String::as_str) != Some(state) {
        return Err(AuthError::StateMismatch);
    }
    if fields.contains_key("error") {
        return Err(AuthError::AuthorizationDenied);
    }
    let code = fields.remove("code").ok_or(AuthError::InvalidCredential)?;
    crate::auth_token::nonempty(&code)?;
    Ok(Secret::new(code))
}
async fn target(stream: &mut TcpStream) -> Result<String, AuthError> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        if bytes.len() == 8192 {
            return Err(AuthError::InvalidCredential);
        }
        bytes.push(stream.read_u8().await.map_err(|_| AuthError::Transport)?);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| AuthError::InvalidCredential)?;
    let mut parts = text.lines().next().unwrap_or_default().split_whitespace();
    if parts.next() != Some("GET") {
        return Err(AuthError::InvalidCredential);
    }
    let target = parts.next().ok_or(AuthError::InvalidCredential)?;
    if !target.starts_with('/') || target.starts_with("//") {
        return Err(AuthError::InvalidCredential);
    }
    Ok(target.to_owned())
}
async fn respond(stream: &mut TcpStream, status: u16) -> Result<(), AuthError> {
    let response =
        format!("HTTP/1.1 {status} Auth\r\nContent-Length: 0\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n");
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|_| AuthError::Transport)
}
pub(super) async fn receive(listener: &TcpListener, state: &str) -> Result<Secret<String>, AuthError> {
    loop {
        let (mut stream, _) = listener.accept().await.map_err(|_| AuthError::Transport)?;
        let target = match target(&mut stream).await {
            Ok(target) => target,
            Err(error) => {
                respond(&mut stream, 400).await?;
                return Err(error);
            }
        };
        if target.split('?').next() != Some("/auth/callback") {
            respond(&mut stream, 404).await?;
            continue;
        }
        let result = code(&target, state);
        respond(&mut stream, if result.is_ok() { 200 } else { 400 }).await?;
        return result;
    }
}
