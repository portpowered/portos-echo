use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

// T3: a real owned child binds an ephemeral loopback socket, announces it
// before listen(), and waits on an independent control connection.
const CHILD: &str = r"
import json, socket, sys
with socket.socket() as host, socket.socket() as control:
    host.bind(('127.0.0.1', 0))
    control.connect(('127.0.0.1', int(sys.argv[1])))
    print(json.dumps({'event':'host.ready','address':'127.0.0.1:'+str(host.getsockname()[1])}), flush=True)
    control.sendall(str(host.getsockname()[1]).encode()+b'\n')
    if control.recv(1) == b'L':
        host.listen()
        control.sendall(b'L')
    sys.stdin.buffer.read()
";

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lab-readiness-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

async fn announced(control: &mut tokio::net::TcpStream) -> std::net::SocketAddr {
    let mut port = vec![];
    loop {
        let byte = control.read_u8().await.unwrap();
        if byte == b'\n' {
            break;
        }
        port.push(byte);
    }
    format!("127.0.0.1:{}", String::from_utf8(port).unwrap())
        .parse()
        .unwrap()
}

#[tokio::test]
async fn t3_readiness_child_announces_before_listening() {
    child_startup(true).await;
}

#[tokio::test]
async fn t3_readiness_child_never_listens_times_out_and_is_reaped() {
    child_startup(false).await;
}

async fn before_startup<T>(
    mut startup: std::pin::Pin<&mut impl std::future::Future<Output = Result<(), String>>>,
    signal: impl std::future::Future<Output = T>,
) -> T {
    tokio::select! {
        result = &mut startup => panic!("startup returned before listener signal: {result:?}"),
        value = signal => value,
    }
}

fn child_command(port: u16) -> Command {
    let mut command = Command::new(if cfg!(windows) { "python" } else { "python3" });
    command.args(["-u", "-c", CHILD, &port.to_string()]);
    command
}

async fn release_listener(control: &mut tokio::net::TcpStream, listen: bool) {
    if listen {
        control.write_all(b"L").await.unwrap();
        assert_eq!(control.read_u8().await.unwrap(), b'L');
    } else {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(20)).await;
    }
}

fn check_startup(
    listen: bool,
    result: &Result<(), String>,
    endpoints: &[std::net::SocketAddr],
    endpoint: std::net::SocketAddr,
    next: usize,
) {
    if listen {
        assert_eq!(result, &Err("launch: fake Live startup failed".into()));
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0], endpoint);
        assert_eq!(next, 1);
    } else {
        assert_eq!(
            result,
            &Err(format!(
                "launch: endpoint {endpoint} readiness timed out after 20000 ms"
            ))
        );
        assert_eq!(endpoints, &[]);
        assert_eq!(next, 0);
    }
}

async fn close_world(world: &mut World, root: &Path) -> Vec<std::net::SocketAddr> {
    let endpoints = world.endpoints.clone();
    let close = world.close().await;
    assert_eq!(world.close().await, Ok(()));
    assert_eq!(close, Ok(()));
    assert!(world.host.is_none());
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    endpoints
}

#[tokio::test]
async fn t3_readiness_cancel_keeps_owned_child_until_explicit_close() {
    let root = Root::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut command = child_command(listener.local_addr().unwrap().port());
    let probed = tokio::sync::Notify::new();
    let mut world = World::default();
    let next = std::cell::Cell::new(0);
    {
        let mut startup = std::pin::pin!(world.start_owned(
            &mut command,
            Path::new("not-launched"),
            &root.0,
            async {
                next.set(next.get() + 1);
                Err(echo_live::call::CallError::Signaling)
            },
            |address| {
                probed.notify_one();
                process::connect(address)
            },
        ));
        let (mut control, _) = before_startup(startup.as_mut(), async { listener.accept().await.unwrap() }).await;
        before_startup(startup.as_mut(), announced(&mut control)).await;
        before_startup(startup.as_mut(), probed.notified()).await;
    }
    assert_eq!(next.get(), 0);
    assert_eq!(world.endpoints, vec![]);
    assert!(!world.host.as_mut().unwrap().exited().unwrap());
    assert_eq!(close_world(&mut world, &root.0).await, vec![]);
}

async fn child_startup(listen: bool) {
    let root = Root::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut command = child_command(listener.local_addr().unwrap().port());
    let probed = tokio::sync::Notify::new();
    let mut world = World::default();
    let next = std::cell::Cell::new(0);
    let result;
    let endpoint;
    {
        let mut startup = std::pin::pin!(world.start_owned(
            &mut command,
            Path::new("not-launched"),
            &root.0,
            async {
                next.set(next.get() + 1);
                Err(echo_live::call::CallError::Signaling)
            },
            |address| {
                probed.notify_one();
                process::connect(address)
            }
        ));
        let (mut control, _) = before_startup(startup.as_mut(), async { listener.accept().await.unwrap() }).await;
        endpoint = before_startup(startup.as_mut(), announced(&mut control)).await;
        before_startup(startup.as_mut(), probed.notified()).await;
        assert_eq!(next.get(), 0);
        assert!(tokio::net::TcpStream::connect(endpoint).await.is_err());
        release_listener(&mut control, listen).await;
        result = startup.await;
    }
    if !listen {
        tokio::time::resume();
    }
    let endpoints = close_world(&mut world, &root.0).await;
    check_startup(listen, &result, &endpoints, endpoint, next.get());
    assert!(tokio::net::TcpStream::connect(endpoint).await.is_err());
}
