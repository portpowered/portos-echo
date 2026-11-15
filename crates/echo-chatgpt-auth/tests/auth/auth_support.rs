use crate::{AuthSource, auth_clock::Clock, auth_private::Directory};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

pub(super) const NOW: u64 = 2_000_000_000;
pub(super) fn token(account: Option<&str>) -> String {
    let mut payload = serde_json::json!({"exp": NOW + 3600});
    if let Some(id) = account {
        payload["https://api.openai.com/auth"] = serde_json::json!({"chatgpt_account_id": id}); // lint-rules: allow-literal inert JWT claim key
    }
    format!("e30.{}.synthetic", URL_SAFE_NO_PAD.encode(payload.to_string()))
}
pub(super) fn tokens(account: Option<&str>) -> String {
    serde_json::json!({"access_token":token(account), "refresh_token":"synthetic-refresh", "expires_in":3600})
        .to_string()
}
pub(super) struct FakeClock(pub(super) AtomicU64);
impl Clock for FakeClock {
    fn unix(&self) -> Result<u64, crate::AuthError> {
        Ok(NOW)
    }
    fn monotonic(&self) -> Duration {
        Duration::from_secs(self.0.load(Ordering::SeqCst))
    }
    fn wait(&self, duration: Duration) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.0.fetch_add(duration.as_secs(), Ordering::SeqCst);
            tokio::task::yield_now().await;
        })
    }
}
#[derive(Debug, Clone)]
pub(super) struct Request {
    pub(super) path: String,
    pub(super) headers: BTreeMap<String, String>,
    pub(super) body: String,
}
async fn read(stream: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    let end = loop {
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 16384);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
    };
    let header = String::from_utf8(bytes).unwrap();
    let mut lines = header.lines();
    let line = lines.next().unwrap();
    assert!(line.starts_with("POST "));
    let path = line.split_whitespace().nth(1).unwrap().to_owned();
    let headers: BTreeMap<_, _> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_lowercase(), v.trim().to_owned()))
        .collect();
    let size: usize = headers["content-length"].parse().unwrap();
    let mut body = vec![0; size];
    stream.read_exact(&mut body).await.unwrap();
    assert!(end < 16384);
    Request {
        path,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}
pub(super) struct Mock {
    pub(super) base: String,
    pub(super) requests: Arc<Mutex<Vec<Request>>>,
    pub(super) seen: tokio::sync::mpsc::Receiver<usize>,
    task: tokio::task::JoinHandle<()>,
}
impl Mock {
    pub(super) async fn start(replies: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut url = reqwest::Url::parse("http://127.0.0.1").unwrap();
        url.set_port(Some(listener.local_addr().unwrap().port())).unwrap();
        let base = url.as_str().trim_end_matches('/').to_owned();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (tx, seen) = tokio::sync::mpsc::channel(256);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read(&mut stream).await;
                let index = {
                    let mut r = captured.lock().unwrap();
                    r.push(request);
                    r.len() - 1
                };
                tx.send(index).await.unwrap();
                let (status, body) = replies.get(index).cloned().unwrap_or((500, "unexpected".into()));
                if status == 0 {
                    std::future::pending::<()>().await;
                }
                let response = format!(
                    "HTTP/1.1 {status} test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self {
            base,
            requests,
            seen,
            task,
        }
    }
    pub(super) fn source(&self, directory: &Directory) -> AuthSource {
        let b = &self.base;
        let config = echo_config::ChatGptAuthConfig {
            issuer: b.clone(),
            client_id: "mock-client".into(),
            scopes: "mock scopes".into(),
            usercode_endpoint: format!("{b}/user"),
            device_token_endpoint: format!("{b}/poll"),
            token_endpoint: format!("{b}/token"),
            authorization_endpoint: format!("{b}/authorize"),
            verification_url: format!("{b}/verify"),
            device_redirect_uri: format!("{b}/callback"),
            browser_redirect_uri: format!("{b}/auth/callback"),
        };
        let mut source = AuthSource::new(config, directory.0.join("own.json")).unwrap();
        source.clock = Arc::new(FakeClock(AtomicU64::new(0)));
        source
    }
    pub(super) async fn stop(self) {
        self.task.abort();
        assert!(self.task.await.unwrap_err().is_cancelled());
    }
}
pub(super) fn directory() -> Directory {
    Directory::new(&std::env::temp_dir()).unwrap()
}
pub(super) fn device() -> (u16, String) {
    (
        200,
        r#"{"device_auth_id":"synthetic-device","user_code":"TEST-CODE","interval":"5"}"#.into(),
    )
}
pub(super) fn approval() -> (u16, String) {
    (
        200,
        r#"{"authorization_code":"synthetic-code","code_verifier":"synthetic-verifier"}"#.into(),
    )
}
pub(super) fn record() -> crate::auth_store::Record {
    crate::auth_token::response(tokens(Some("synthetic-account")).as_bytes(), NOW).unwrap()
}
