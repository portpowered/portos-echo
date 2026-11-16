//! External replay actor: reads synthetic private state, never opens a store writer.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use echo_contracts::secret::Secret;
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const ERROR: &str = "M5: replay protocol mismatch";

pub async fn reject(initial: &Secret<Value>, now: u64, link: u64) -> Result<(), String> {
    let record = initial.expose();
    let issuer = record["issuer"].as_str().ok_or(ERROR)?;
    let metadata = format!("{issuer}/.well-known/oauth-authorization-server");
    let (status, discovery) = request("GET", &metadata, "", "").await?;
    if status != 200 || discovery.expose()["issuer"] != issuer {
        return Err(ERROR.into());
    }
    let endpoint = discovery.expose()["token_endpoint"].as_str().ok_or(ERROR)?;
    let url = local(endpoint)?;
    if url.origin() != local(issuer)?.origin() {
        return Err(ERROR.into());
    }
    let proof = proof(record, endpoint, now, link)?;
    let body = Secret::new(
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("client_id", record["client_id"].as_str().ok_or(ERROR)?)
            .append_pair("refresh_token", record["grant"]["refresh_token"].as_str().ok_or(ERROR)?)
            .finish(),
    );
    let (status, value) = request("POST", endpoint, proof.expose(), body.expose()).await?;
    if status != 400 || value.expose() != &json!({"error":"invalid_grant"}) {
        return Err(ERROR.into());
    }
    Ok(())
}

fn proof(record: &Value, endpoint: &str, now: u64, link: u64) -> Result<Secret<String>, String> {
    let jwk = &record["private_key_jwk"];
    let private = Secret::new(B64.decode(jwk["d"].as_str().ok_or(ERROR)?).map_err(|_| ERROR)?);
    let key = SigningKey::from_slice(private.expose()).map_err(|_| ERROR)?;
    let header = json!({"typ":"dpop+jwt","alg":"ES256",
        "jwk":{"kty":"EC","crv":"P-256","x":jwk["x"],"y":jwk["y"]}});
    let claims = json!({"htm":"POST","htu":endpoint,"iat":now,"jti":format!("m5-external-replay-{link}")});
    let input = format!("{}.{}", B64.encode(header.to_string()), B64.encode(claims.to_string()));
    let signature: Signature = key.sign(input.as_bytes());
    Ok(Secret::new(format!("{input}.{}", B64.encode(signature.to_bytes()))))
}

fn local(value: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(value).map_err(|_| ERROR)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none_or(|port| port == 0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ERROR.into());
    }
    Ok(url)
}

async fn request(method: &str, endpoint: &str, proof: &str, body: &str) -> Result<(u16, Secret<Value>), String> {
    let url = local(endpoint)?;
    let port = url.port().ok_or(ERROR)?;
    let mut socket = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|_| ERROR)?;
    let header = if proof.is_empty() {
        String::new()
    } else {
        format!("DPoP: {proof}\r\n")
    };
    let wire = Secret::new(format!(
        "{method} {} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\
         Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n{header}\r\n{body}",
        url.path(),
        body.len()
    ));
    socket.write_all(wire.expose().as_bytes()).await.map_err(|_| ERROR)?;
    let mut bytes = Vec::new();
    socket.take(65537).read_to_end(&mut bytes).await.map_err(|_| ERROR)?;
    if bytes.len() > 65536 {
        return Err(ERROR.into());
    }
    let end = bytes.windows(4).position(|part| part == b"\r\n\r\n").ok_or(ERROR)?;
    let head = std::str::from_utf8(&bytes[..end]).map_err(|_| ERROR)?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("HTTP/1.1 "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or(ERROR)?
        .parse()
        .map_err(|_| ERROR)?;
    let value = serde_json::from_slice(&bytes[end + 4..])
        .map(Secret::new)
        .map_err(|_| ERROR)?;
    Ok((status, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn discovery_peer(listener: tokio::net::TcpListener, metadata: Value, status: u16) -> Vec<String> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (socket, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(socket);
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            lines.push(line);
        }
        let body = metadata.to_string();
        reader
            .get_mut()
            .write_all(
                format!(
                    "HTTP/1.1 {status} Synthetic\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        lines
    }

    // T3: replay discovers endpoints without carrying the consumed credential
    // or proof, and refuses bad discovery before attempting a token request.
    #[tokio::test]
    async fn t3_replay_refuses_bad_discovery_without_disclosing_the_grant() {
        use tokio::net::TcpListener;
        for fault in ["status", "issuer", "remote", "origin", "missing", "key"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            assert_eq!(address.ip(), std::net::Ipv4Addr::LOCALHOST);
            let mut endpoint = url::Url::parse("http://127.0.0.1").unwrap();
            endpoint.set_port(Some(address.port())).unwrap();
            let issuer = endpoint.as_str().trim_end_matches('/').to_owned();
            endpoint.set_path("/discovered/token");
            if fault == "remote" {
                endpoint.set_host(Some("service.example.invalid")).unwrap();
            }
            if fault == "origin" {
                endpoint.set_port(Some(1)).unwrap();
            }
            let mut metadata = json!({"issuer":issuer,"token_endpoint":endpoint.as_str()});
            if fault == "issuer" {
                metadata["issuer"] = json!(format!("{issuer}/wrong-issuer"));
            }
            if fault == "missing" {
                metadata.as_object_mut().unwrap().remove("token_endpoint");
            }
            let status = if fault == "status" { 503 } else { 200 };
            let peer = tokio::spawn(discovery_peer(listener, metadata, status));
            let initial = Secret::new(json!({"issuer":issuer,
                "grant":{"refresh_token":"synthetic-never-disclosed"}}));
            assert_eq!(reject(&initial, 1_800_000_005, 1).await, Err(ERROR.into()), "{fault}");
            assert_eq!(
                peer.await.unwrap(),
                vec![
                    "GET /.well-known/oauth-authorization-server HTTP/1.1\r\n".to_owned(),
                    format!("Host: 127.0.0.1:{}\r\n", address.port()),
                    "Connection: close\r\n".to_owned(),
                    "Content-Type: application/x-www-form-urlencoded\r\n".to_owned(),
                    "Content-Length: 0\r\n".to_owned(),
                ]
            );
        }
    }
}
