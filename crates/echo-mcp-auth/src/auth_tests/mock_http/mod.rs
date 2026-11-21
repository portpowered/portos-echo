use super::{Request, invalid};
use std::collections::BTreeMap;
use tokio::{io::AsyncReadExt, net::TcpStream, time::Instant};

/// The request path and its decoded query parameters.
fn split_target(target: &str) -> (String, BTreeMap<String, String>) {
    match target.split_once('?') {
        Some((path, query)) => (
            path.to_owned(),
            url::form_urlencoded::parse(query.as_bytes()).into_owned().collect(),
        ),
        None => (target.to_owned(), BTreeMap::new()),
    }
}

pub(super) async fn read(stream: &mut TcpStream, start: Instant) -> std::io::Result<Request> {
    let mut bytes = Vec::new();
    let end = loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).await?;
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
        if bytes.len() >= 16384 {
            return Err(invalid());
        }
    };
    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
    let mut lines = text.split("\r\n");
    let mut line = lines.next().ok_or_else(invalid)?.split_whitespace();
    let method = line.next().ok_or_else(invalid)?.into();
    let (path, query) = split_target(line.next().ok_or_else(invalid)?);
    let headers: BTreeMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.to_lowercase(), v.trim().into()))
        .collect();
    let length = headers
        .get("content-length")
        .map_or(Ok(0), |v| v.parse::<usize>().map_err(|_| invalid()))?;
    if length > 8 * 1024 * 1024 {
        return Err(invalid());
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    if end >= 16384 {
        return Err(invalid());
    }
    let received = Instant::now();
    let json = serde_json::from_slice(&body).ok();
    let form_all: Vec<(String, String)> = if json.is_none() {
        url::form_urlencoded::parse(&body).into_owned().collect()
    } else {
        Vec::new()
    };
    Ok(Request {
        method,
        message_body: (path == "/messages").then(|| body.into()),
        path,
        headers,
        form: form_all.iter().cloned().collect(),
        form_all,
        query,
        json,
        time: (received - start).as_secs(),
        received,
        status: None,
    })
}
