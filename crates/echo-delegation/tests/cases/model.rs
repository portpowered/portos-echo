//! T3 loopback fixture with explicit request, release and disconnect signals.
use crate::support::{Request, read};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};

pub struct Reply {
    pub status: u16,
    pub body: String,
    pub stall: bool,
}
impl Reply {
    pub fn body(body: String) -> Self {
        Self {
            status: 200,
            body,
            stall: false,
        }
    }
    pub fn stalled() -> Self {
        Self {
            status: 200,
            body: String::new(),
            stall: true,
        }
    }
}
pub struct Model {
    pub base: String,
    entered: mpsc::Receiver<()>,
    closed: mpsc::Receiver<()>,
    requests: Arc<Mutex<Vec<Request>>>,
    shutdown: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<()>>,
}
impl Model {
    pub async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port()); // lint-rules: allow-literal T3 numeric loopback fixture origin
        let (entered_tx, entered) = mpsc::channel(32);
        let (closed_tx, closed) = mpsc::channel(32);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let (shutdown, mut stop) = oneshot::channel();
        let worker = tokio::spawn(async move {
            let mut workers = JoinSet::new();
            let mut replies = replies.into_iter();
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stop => break,
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        let reply = replies.next().expect("unexpected model POST");
                        let recorded = recorded.clone();
                        let entered = entered_tx.clone();
                        let closed = closed_tx.clone();
                        workers.spawn(async move {
                            let request = read(&mut socket).await;
                            recorded.lock().unwrap().push(request);
                            entered.send(()).await.unwrap();
                            if reply.stall {
                                assert_eq!(socket.read(&mut [0; 1]).await.unwrap(), 0);
                                closed.send(()).await.unwrap();
                            } else {
                                let header = format!("HTTP/1.1 {} fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.status, reply.body.len());
                                socket.write_all(header.as_bytes()).await.unwrap();
                                socket.write_all(reply.body.as_bytes()).await.unwrap();
                                socket.shutdown().await.unwrap();
                            }
                        });
                    }
                }
            }
            workers.abort_all();
            while let Some(result) = workers.join_next().await {
                if let Err(error) = result {
                    assert!(error.is_cancelled(), "{error}");
                }
            }
        });
        Self {
            base,
            entered,
            closed,
            requests,
            shutdown: Some(shutdown),
            worker: Some(worker),
        }
    }
    pub async fn entered(&mut self) {
        self.entered.recv().await.unwrap();
    }
    pub async fn closed(&mut self) {
        self.closed.recv().await.unwrap();
    }
    pub async fn finish(mut self) -> Vec<Request> {
        self.shutdown.take().unwrap().send(()).unwrap();
        self.worker.take().unwrap().await.unwrap();
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}
impl Drop for Model {
    fn drop(&mut self) {
        self.shutdown.take();
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}
pub fn final_item(text: &str) -> Value {
    serde_json::json!({"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":text}]})
}
