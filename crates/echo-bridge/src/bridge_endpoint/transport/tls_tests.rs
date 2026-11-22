//! T3: the endpoint's REST exchange and gateway upgrade trust the configured
//! bundle. A loopback TLS peer with a synthetic certificate stands in for an
//! Echo, which has no native roots: without the bundle both handshakes fail
//! as `tls`, with it both succeed.
use super::*;
use echo_contracts::{endpoint::InstallRequest, secret::Secret};
use echo_mcp_auth::{EndpointExchange, ExchangeError, TokenFuture, Unanswered};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Answers the exchange and accepts one WebSocket upgrade per connection.
const PEER: &str = r"
import base64, hashlib, http.server, json, ssl, sys, threading
class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *_): pass
    def do_POST(self):
        self.rfile.read(int(self.headers['Content-Length']))
        data = json.dumps(dict(authLinkId='synthetic-link', id='exchange-1', etag='v1', status='SUCCEEDED')).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)
    def do_GET(self):
        key = self.headers['Sec-WebSocket-Key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
        self.send_response(101)
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', base64.b64encode(hashlib.sha1(key.encode()).digest()).decode())
        self.end_headers()
        self.close_connection = True
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain('ca.pem', 'key.pem')
server.socket = context.wrap_socket(server.socket, server_side=True)
threading.Thread(target=server.serve_forever, daemon=True).start()
print(server.server_address[1], flush=True)
sys.stdin.read()
";

struct Token;

impl EndpointTokenSource for Token {
    fn access_token(&self) -> TokenFuture<'_> {
        Box::pin(async { Ok(Secret::new("synthetic-endpoint-access".into())) })
    }
}

#[tokio::test]
async fn t3_endpoint_exchange_and_gateway_trust_the_configured_bundle() {
    let directory = tempfile::tempdir().unwrap();
    crate::bridge_mcp::bridge_component::certificate(directory.path(), false);
    let mut peer = tokio::process::Command::new("python3")
        .args(["-u", "-c", PEER])
        .current_dir(directory.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut port = String::new();
    BufReader::new(peer.stdout.take().unwrap())
        .read_line(&mut port)
        .await
        .unwrap();
    let port: u16 = port.trim().parse().unwrap();
    let api = format!("https://127.0.0.1:{port}"); // lint-rules: allow-literal loopback TLS peer
    let gateway = format!("wss://127.0.0.1:{port}/devices/{{authLinkId}}"); // lint-rules: allow-literal loopback TLS peer
    let bundle = std::fs::read(directory.path().join("ca.pem")).unwrap();
    let install = InstallRequest::new("00000000-0000-4000-8000-000000000001", "Hall Echo", None);
    let key = "00000000-0000-4000-8000-000000000002";

    let untrusted = EndpointExchange::open(&api, REQUEST_TIMEOUT, None).unwrap();
    let refused = untrusted.submit(&Token, key, &install).await.err().unwrap();
    assert_eq!(refused, ExchangeError::Transport(Unanswered::Tls));
    assert_eq!((refused.reason(), refused.retryable()), ("tls", true));
    let trusted = EndpointExchange::open(&api, REQUEST_TIMEOUT, Some(&bundle)).unwrap();
    let installed = trusted.submit(&Token, key, &install).await.unwrap();
    assert_eq!(installed.auth_link_id, "synthetic-link");

    let untrusted = Transport::new(&gateway, &api, false, None).unwrap();
    let refused = untrusted
        .handshake("synthetic-link", &Token, false)
        .await
        .err()
        .unwrap();
    assert_eq!(
        (refused.status, refused.reason, refused.http_status),
        (Status::Connecting, "tls", None)
    );
    let trusted = Transport::new(&gateway, &api, false, Some(&bundle)).unwrap();
    assert!(trusted.handshake("synthetic-link", &Token, false).await.is_ok());

    for pem in [b"".as_slice(), b"not PEM"] {
        assert!(Transport::new(&gateway, &api, false, Some(pem)).is_none());
    }
    peer.stdin.take().unwrap().shutdown().await.unwrap();
    assert!(peer.wait().await.unwrap().success());
}
