use super::{
    api,
    config::Config,
    control, ipc,
    state::{Service, ServiceClock, ServiceStore},
};
use crate::{CliGadget, MuseBackend};
use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{TcpListener, UnixListener},
    signal::unix::{SignalKind, signal},
    sync::Semaphore,
    task::JoinSet,
};
use tokio_rustls::TlsAcceptor;

/// Run the explicitly configured Linux service; no installation or defaults.
/// # Errors
/// Returns a stable diagnostic code without secret or task data.
pub fn run() -> Result<(), &'static str> {
    let (path, control) = arguments()?;
    let config = Config::load(&path, control)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "runtime_unavailable")?;
    let result = runtime.block_on(start(config, control));
    // Signal shutdown must not wait for Tokio's blocking stdin reader.
    runtime.shutdown_timeout(Duration::from_millis(100));
    result
}

fn arguments() -> Result<(PathBuf, bool), &'static str> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(args.len() == 2 || args.len() == 3)
        || args[0] != "--config"
        || (args.len() == 3 && args[2] != "--control-stdin")
    {
        return Err("invalid_arguments");
    }
    Ok((PathBuf::from(&args[1]), args.len() == 3))
}

struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _removed = std::fs::remove_file(&self.0);
    }
}

async fn start(config: Config, controlled: bool) -> Result<(), &'static str> {
    let tls = TlsAcceptor::from(config.tls()?);
    let credential = config.credential()?;
    let store = ServiceStore::open(&config.store)?;
    // A stale socket is not removed speculatively: it may have another owner.
    let socket = UnixListener::bind(&config.report_socket).map_err(|_| "report_socket_unavailable")?;
    let _socket_guard = SocketGuard(config.report_socket.clone());
    std::fs::set_permissions(&config.report_socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| "report_socket_unavailable")?;
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|_| "listener_unavailable")?;
    let clock = ServiceClock::new(&config.clock);
    let service = Arc::new(Service {
        backend: MuseBackend::new(
            CliGadget {
                program: config.gadget_program,
            },
            clock.clone(),
            store.clone(),
        ),
        jobs: std::sync::Mutex::new(Vec::new()),
        clock: clock.clone(),
        store,
        credential,
        children: Semaphore::new(4),
        stopping: AtomicBool::new(false),
    });
    accept_loop(listener, socket, tls, service, clock, controlled).await?;
    std::fs::remove_file(&config.report_socket).map_err(|_| "socket_cleanup_failed")?;
    Ok(())
}

async fn accept_loop(
    listener: TcpListener,
    socket: UnixListener,
    tls: TlsAcceptor,
    service: Arc<Service>,
    clock: ServiceClock,
    controlled: bool,
) -> Result<(), &'static str> {
    let mut interrupt = signal(SignalKind::interrupt()).map_err(|_| "signal_unavailable")?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|_| "signal_unavailable")?;
    let address = listener.local_addr().map_err(|_| "listener_unavailable")?;
    writeln!(std::io::stdout(), "{{\"ready\":true,\"listen\":\"{address}\"}}").map_err(|_| "readiness_failed")?;
    let admin = async {
        if controlled {
            control::control(clock).await
        } else {
            std::future::pending::<Result<(), &'static str>>().await
        }
    };
    tokio::pin!(admin);
    let connections = Arc::new(Semaphore::new(32));
    let mut tasks = JoinSet::new();
    let result = loop {
        tokio::select! {
            result = &mut admin => break result,
            _ = interrupt.recv() => break Ok(()),
            _ = terminate.recv() => break Ok(()),
            Some(_) = tasks.join_next() => {},
            incoming = listener.accept() => {
                let Ok((stream, _)) = incoming else { break Err("listener_failed"); };
                if let Ok(permit) = connections.clone().try_acquire_owned() {
                    let (tls, service) = (tls.clone(), service.clone());
                    tasks.spawn(async move { api::serve(stream, tls, service, permit).await; });
                }
            }
            incoming = socket.accept() => {
                let Ok((stream, _)) = incoming else { break Err("report_socket_failed"); };
                if let Ok(permit) = connections.clone().try_acquire_owned() {
                    let service = service.clone();
                    tasks.spawn(async move { let _permit = permit; ipc::serve(stream, service).await; });
                }
            }
        }
    };
    service.stopping.store(true, Ordering::SeqCst);
    drop(listener);
    drop(socket);
    while tasks.join_next().await.is_some() {}
    drain(&service).await?;
    result
}

async fn drain(service: &Service) -> Result<(), &'static str> {
    let jobs = std::mem::take(&mut *service.jobs.lock().map_err(|_| "worker_failed")?);
    for job in jobs {
        job.await.map_err(|_| "worker_failed")?;
    }
    if !service.store.healthy() {
        return Err("storage_unavailable");
    }
    Ok(())
}
