use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use echo_config::{ChatGptAuthConfig, LiveConfig, LiveMode};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

pub const ACCOUNT: &str = "synthetic-account-1234";
pub const REFRESH: &str = "synthetic-refresh";
static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> std::io::Result<Self> {
        loop {
            let path = std::env::temp_dir().join(format!(
                "echo-signin-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }
    pub fn live(&self, mode: LiveMode) -> LiveConfig {
        LiveConfig {
            mode,
            credential_file: self.0.join("credential"),
            voice: None,
            session_idle_seconds: 60,
        }
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

pub fn token() -> String {
    let payload = serde_json::json!({
        "exp":4_000_000_000_u64,
        "https://api.openai.com/auth":{"chatgpt_account_id":ACCOUNT}, // lint-rules: allow-literal inert JWT claim key
    });
    format!("e30.{}.synthetic", URL_SAFE_NO_PAD.encode(payload.to_string()))
}
pub fn approved() -> Vec<(u16, String)> {
    vec![
        (
            200,
            r#"{"device_auth_id":"synthetic-device","user_code":"TEST-CODE","interval":1}"#.into(),
        ),
        (
            200,
            r#"{"authorization_code":"synthetic-code","code_verifier":"synthetic-verifier"}"#.into(),
        ),
        (
            200,
            serde_json::json!({"access_token":token(), "refresh_token":REFRESH}).to_string(),
        ),
    ]
}

pub struct Request {
    pub path: String,
    pub body: String,
}
async fn request(stream: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 16_384);
    }
    let header = String::from_utf8(bytes).unwrap();
    let path = header
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let length: usize = header
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .unwrap()
        .1
        .trim()
        .parse()
        .unwrap();
    assert!(length < 16_384);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await.unwrap();
    Request {
        path,
        body: String::from_utf8(body).unwrap(),
    }
}

pub struct Mock {
    pub config: ChatGptAuthConfig,
    pub requests: Arc<Mutex<Vec<Request>>>,
    pub seen: tokio::sync::mpsc::Receiver<usize>,
    pub events: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Mock {
    pub async fn start(replies: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = "http://127.0.0.1";
        let base = format!("{origin}:{}", listener.local_addr().unwrap().port());
        let config = ChatGptAuthConfig {
            issuer: base.clone(),
            client_id: "mock-client".into(),
            scopes: "mock scopes".into(),
            usercode_endpoint: format!("{base}/user"),
            device_token_endpoint: format!("{base}/poll"),
            token_endpoint: format!("{base}/token"),
            authorization_endpoint: format!("{base}/authorize"),
            verification_url: format!("{base}/verify"),
            device_redirect_uri: format!("{base}/callback"),
            browser_redirect_uri: format!("{base}/auth/callback"),
        };
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ordered = events.clone();
        let (tx, seen) = tokio::sync::mpsc::channel(16);
        let task = tokio::spawn(async move {
            for (index, (status, body)) in replies.into_iter().enumerate() {
                let (mut stream, _) = listener.accept().await.unwrap();
                let received = request(&mut stream).await;
                ordered.lock().unwrap().push(received.path.clone());
                captured.lock().unwrap().push(received);
                tx.send(index).await.unwrap();
                if status == 0 {
                    continue;
                }
                if status == 1 {
                    std::future::pending::<()>().await;
                }
                let reply = format!(
                    "HTTP/1.1 {status} test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
            std::future::pending::<()>().await;
        });
        Self {
            config,
            requests,
            seen,
            events,
            task,
        }
    }
    pub async fn stop(self) {
        self.task.abort();
        assert!(self.task.await.unwrap_err().is_cancelled());
    }
}

pub struct Writer {
    pub bytes: Vec<u8>,
    pub limit: usize,
    pub events: Arc<Mutex<Vec<String>>>,
}
impl Writer {
    pub fn new(mock: &Mock, limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            events: mock.events.clone(),
        }
    }
}
impl std::io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len() >= self.limit {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        let count = bytes.len().min(self.limit - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.events.lock().unwrap().push("output".into());
        Ok(())
    }
}
