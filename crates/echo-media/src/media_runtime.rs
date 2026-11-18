// Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
// Adapted from Codex voice-host transport_runtime.rs; Modified by Port OS.
//! Bounds retained inbound TCP streams across one peer's listeners before authentication.
//! Weak references reuse slots after stream ownership ends; other Tokio I/O stays unchanged.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::time::Duration;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
use webrtc::runtime::AsyncInterval;
use webrtc::runtime::AsyncTcpListener;
use webrtc::runtime::AsyncTcpStream;
use webrtc::runtime::AsyncUdpSocket;
use webrtc::runtime::JoinHandle;
use webrtc::runtime::Runtime;
use webrtc::runtime::TokioRuntime;

const MAX_INBOUND_STREAMS: usize = 32;
type AcceptedStreams = Arc<Mutex<Vec<Weak<dyn AsyncTcpStream>>>>;

#[derive(Debug, Default)]
pub(crate) struct MediaRuntime {
    accepted: AcceptedStreams,
    #[cfg(test)]
    pub(crate) udp: Mutex<Vec<Weak<dyn AsyncUdpSocket>>>,
    #[cfg(test)]
    pub(crate) tcp: Mutex<Vec<Weak<dyn AsyncTcpListener>>>,
}

impl Runtime for MediaRuntime {
    fn spawn(&self, future: BoxFuture<'static, ()>) -> Box<dyn JoinHandle> {
        TokioRuntime.spawn(future)
    }

    fn spawn_reactor(&self, reactor_pool_size: usize, future: BoxFuture<'static, ()>) -> Box<dyn JoinHandle> {
        TokioRuntime.spawn_reactor(reactor_pool_size, future)
    }

    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        let socket = TokioRuntime.wrap_udp_socket(socket)?;
        #[cfg(test)]
        self.udp.lock().unwrap().push(Arc::downgrade(&socket));
        Ok(socket)
    }

    fn wrap_tcp_listener(&self, listener: std::net::TcpListener) -> io::Result<Arc<dyn AsyncTcpListener>> {
        let listener = TokioRuntime.wrap_tcp_listener(listener)?;
        #[cfg(test)]
        self.tcp.lock().unwrap().push(Arc::downgrade(&listener));
        Ok(Arc::new(LimitedListener {
            inner: listener,
            accepted: self.accepted.clone(),
        }))
    }

    fn connect_tcp(&self, remote_addr: SocketAddr) -> BoxFuture<'_, io::Result<Arc<dyn AsyncTcpStream>>> {
        TokioRuntime.connect_tcp(remote_addr)
    }

    fn resolve_host<'a>(&'a self, host: &'a str) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        TokioRuntime.resolve_host(host)
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        TokioRuntime.sleep(duration)
    }

    fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
        TokioRuntime.interval(period)
    }

    fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
        TokioRuntime.block_on(future);
    }

    fn yield_now(&self) -> BoxFuture<'static, ()> {
        TokioRuntime.yield_now()
    }

    fn now(&self) -> std::time::Instant {
        tokio::time::Instant::now().into_std()
    }

    fn name(&self) -> &'static str {
        TokioRuntime.name()
    }
}

#[derive(Debug)]
struct LimitedListener {
    inner: Arc<dyn AsyncTcpListener>,
    accepted: AcceptedStreams,
}

impl AsyncTcpListener for LimitedListener {
    fn accept(&self) -> BoxFuture<'_, io::Result<(Arc<dyn AsyncTcpStream>, SocketAddr)>> {
        Box::pin(async move {
            let (stream, address) = self.inner.accept().await?;
            let mut accepted = self
                .accepted
                .lock()
                .map_err(|_| io::Error::other("voice TCP admission unavailable"))?;
            accepted.retain(|stream| stream.strong_count() > 0);
            if accepted.len() >= MAX_INBOUND_STREAMS {
                return Err(io::Error::other("voice TCP connection limit reached"));
            }
            accepted.push(Arc::downgrade(&stream));
            Ok((stream, address))
        })
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}
