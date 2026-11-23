//! Loopback setup fixture shared by the CLI and installer transaction tests.
use std::{
    fs,
    io::{self, Read, Write},
    net::TcpListener,
    thread,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Service {
    pub(crate) address: std::net::SocketAddr,
    screen_address: std::net::SocketAddr,
    screen_worker: Option<thread::JoinHandle<io::Result<()>>>,
    worker: Option<thread::JoinHandle<io::Result<()>>>,
}

impl Service {
    pub fn new(root: &std::path::Path) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let screen = TcpListener::bind("127.0.0.1:0")?;
        let screen_address = screen.local_addr()?;
        let replies: Vec<_> = (0..8)
            .flat_map(|_| [Ok(screen_address.port().to_string()), Ok(address.port().to_string())])
            .collect::<io::Result<_>>()?;
        fs::write(root.join("forward-replies.json"), serde_json::to_vec(&replies)?)?;
        fs::write(root.join("forwards.json"), "[]")?;
        fs::write(root.join("forward-targets.json"), "{}")?;
        fs::write(root.join("stop-reply"), "echo-install: stopped\r\n")?;
        for verb in ["ensure-signin", "signin", "link"] {
            fs::write(
                root.join(format!("{verb}-reply")),
                format!("echo-install: {verb} completed\r\n"),
            )?;
        }
        fs::write(
            root.join("setup-config"),
            include_str!("../../../../examples/bridge.echo.toml"),
        )?;
        fs::write(
            root.join("confirm-cursor"),
            format!("1:2:0:{}\n1:3:0:{}\n", "a".repeat(64), "b".repeat(64)), // lint-rules: allow-literal inert opaque paired cursors
        )?;
        fs::write(
            root.join("confirm-evidence"),
            "wake=confirmed\r\nlink=connected\r\nrun=started\r\n",
        )?;
        fs::write(root.join("setup-pairing"), "SYNTHETIC-PAIR\n")?;
        let health_root = root.to_owned();
        let screen_root = root.to_owned();
        let worker = thread::spawn(move || serve(&listener, &health_root));
        let screen_worker = thread::spawn(move || serve(&screen, &screen_root));
        Ok(Self {
            address,
            screen_address,
            screen_worker: Some(screen_worker),
            worker: Some(worker),
        })
    }
}

fn serve(listener: &TcpListener, root: &std::path::Path) -> io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if stream.read_exact(&mut byte).is_err() {
                break;
            }
            request.push(byte[0]);
        }
        if !request.ends_with(b"\r\n\r\n") {
            continue;
        }
        if request.starts_with(b"GET /events?token=SYNTHETIC-PAIR ") {
            // Immediate account completion can cancel the observer at any point.
            // Keep its SSE stream pending until that owner drops the response.
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
            let mut byte = [0];
            let _ = stream.read(&mut byte);
            continue;
        }
        if request.starts_with(b"GET /stop ") {
            return Ok(());
        }
        let settings = request.starts_with(b"GET /settings/v1?token=SYNTHETIC-PAIR ");
        assert!(settings || request.starts_with(b"GET /healthz "));
        let body = if settings {
            serde_json::json!({"version":1,"link_status":fs::read_to_string(root.join("link-state")).unwrap_or_else(|_| "link_required".into())}).to_string()
        } else {
            serde_json::json!({"status":"running", "wyoming":"connected", "live":"idle", "relink_required":false, "kill_switch":false}).to_string()
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )?;
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Ok(mut socket) = std::net::TcpStream::connect(self.address) {
            let _ = socket.write_all(b"GET /stop HTTP/1.1\r\n\r\n");
        }
        if let Ok(mut socket) = std::net::TcpStream::connect(self.screen_address) {
            let _ = socket.write_all(b"GET /stop HTTP/1.1\r\n\r\n");
        }
        if let Some(worker) = self.screen_worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
