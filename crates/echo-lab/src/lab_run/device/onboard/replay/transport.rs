//! Python's standard TLS client: no proxy, redirects or credential-bearing argv.
use super::{ERROR, Secret, Value, https, json};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SCRIPT: &str = r#"
import http.client, json, ssl, sys, urllib.parse
try:
    value = json.load(sys.stdin)
    url = urllib.parse.urlsplit(value['url'])
    context = ssl.create_default_context(cadata=value['pem'])
    connection = http.client.HTTPSConnection(url.hostname, url.port, timeout=10, context=context)
    headers = {'Content-Type': 'application/x-www-form-urlencoded'}
    if value['proof']:
        headers['DPoP'] = value['proof']
    connection.request(value['method'], url.path or '/', body=value['body'].encode(), headers=headers)
    response = connection.getresponse()
    body = response.read(65537)
    connection.close()
    if len(body) > 65536:
        raise ValueError()
    print(json.dumps({'status': response.status, 'body': json.loads(body)}))
except Exception:
    sys.exit(1)
"#;

pub(super) async fn request(
    python: &str,
    method: &str,
    endpoint: &str,
    proof: &str,
    body: &str,
    pem: &[u8],
) -> Result<(u16, Secret<Value>), String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(12),
        exchange(python, method, endpoint, proof, body, pem),
    )
    .await
    .map_err(|_| ERROR)?
}

async fn exchange(
    python: &str,
    method: &str,
    endpoint: &str,
    proof: &str,
    body: &str,
    pem: &[u8],
) -> Result<(u16, Secret<Value>), String> {
    https(endpoint)?;
    let pem = std::str::from_utf8(pem).map_err(|_| ERROR)?;
    let input = Secret::new(json!({"url":endpoint,"method":method,"proof":proof,"body":body,"pem":pem}).to_string());
    let mut child = tokio::process::Command::new(python)
        .args(["-I", "-c", SCRIPT])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ERROR)?;
    let mut stdin = child.stdin.take().ok_or(ERROR)?;
    stdin.write_all(input.expose().as_bytes()).await.map_err(|_| ERROR)?;
    drop(stdin);
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .ok_or(ERROR)?
        .take(131073)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ERROR)?;
    let output = Secret::new(bytes);
    if output.expose().len() > 131072 || !child.wait().await.map_err(|_| ERROR)?.success() {
        return Err(ERROR.into());
    }
    let value: Secret<Value> = Secret::new(serde_json::from_slice(output.expose()).map_err(|_| ERROR)?);
    let status = value.expose()["status"]
        .as_u64()
        .and_then(|v| u16::try_from(v).ok())
        .ok_or(ERROR)?;
    Ok((status, Secret::new(value.expose()["body"].clone())))
}
