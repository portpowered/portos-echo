//! Bounded, transparent Wyoming observer. Times are taken after successful
//! socket writes, never from capture FIFO writes or playback-file growth.
use std::{net::SocketAddr, time::Duration};
mod earcon;
mod reconnect;
mod session;

use echo_contracts::wyoming::{Decoder, Event, Header, WyomingEvent};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

#[derive(Clone, Debug, Default)]
pub struct Timing {
    session: session::Session,
    pub captured: usize,
    captured_chunks: usize,
    endpoint_sample: Option<usize>,
    capture_tail: Vec<u8>,
    earcon_in_capture: bool,
    pub played: usize,
    pub nonzero_samples: usize,
    pub audible_samples: usize,
    earcon: earcon::Earcon,
    endpoint: Option<Duration>,
    first_pcm: Option<Duration>,
    wake: Option<Duration>,
    run: bool,
    failure: Option<String>,
}

impl Timing {
    pub fn capture_observation(&self) -> serde_json::Value {
        serde_json::json!({"bytes":self.captured,"chunks":self.captured_chunks,
            "endpoint_sample":self.endpoint_sample,"earcon_first_frame_present":self.earcon_in_capture})
    }

    pub fn endpoint_seen(&self) -> bool {
        self.endpoint.is_some()
    }

    pub fn wake_observation(&self) -> serde_json::Value {
        serde_json::json!({"earcon_bytes":self.earcon.bytes,
            "wake_to_earcon_ms":self.earcon.first.zip(self.wake)
                .and_then(|(first, wake)| first.checked_sub(wake)).map(|time| time.as_millis())})
    }

    fn capture(&mut self, event: &Event, target: usize, now: Duration) -> Result<(), String> {
        self.session.capture(event, now);
        match event.header.r#type.as_str() {
            "run-pipeline" if self.run => return Err("wire: multiple runs in one world".into()),
            "run-pipeline" => {
                self.run = true;
                self.wake = Some(now);
            }
            "audio-chunk" if self.run => {
                if !pcm(event, 16000) {
                    return Err("wire: invalid capture PCM".into());
                }
                self.captured_chunks += 1;
                self.capture_tail.extend_from_slice(&event.payload);
                self.earcon_in_capture |= self
                    .capture_tail
                    .windows(960)
                    .step_by(2)
                    .any(|bytes| earcon::matches(bytes, 0));
                let discard = self.capture_tail.len().saturating_sub(958);
                self.capture_tail.drain(..discard);
                self.captured = self
                    .captured
                    .checked_add(event.payload.len())
                    .ok_or("wire: capture overflow")?;
                if self.endpoint.is_none() && self.captured > target {
                    return Err("wire: utterance endpoint is not a frame boundary".into());
                }
                if self.endpoint.is_none() && self.captured >= target {
                    self.endpoint = Some(now);
                    self.endpoint_sample = Some(target / 2);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn playback(&mut self, event: &Event, now: Duration) -> Result<(), String> {
        let earcon = self.earcon.observe(event, now)?;
        self.session.playback(event, now, earcon);
        if event.header.r#type == "audio-chunk" {
            if !self.run || !pcm(event, 24000) {
                return Err("wire: invalid reply PCM".into());
            }
            if earcon {
                return Ok(());
            }
            self.played = self
                .played
                .checked_add(event.payload.len())
                .ok_or("wire: playback overflow")?;
            for bytes in event.payload.as_chunks::<2>().0 {
                let sample = i16::from_le_bytes(*bytes);
                self.nonzero_samples += usize::from(sample != 0);
                self.audible_samples += usize::from(sample.unsigned_abs() > 64);
            }
            self.first_pcm.get_or_insert(now);
        }
        Ok(())
    }

    pub fn audible_reply(&self, host: &serde_json::Value) -> Result<serde_json::Value, String> {
        let measured = serde_json::json!({"pcm_bytes":self.played,
            "nonzero_samples":self.nonzero_samples,"audible_samples":self.audible_samples,
            "wake":self.wake_observation(),
            "capture":self.capture_observation(),
            "session":self.session.observation(),
            "host":host});
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        audible_measurement(&measured)?;
        Ok(measured)
    }

    pub fn sample(&self) -> Result<Duration, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let endpoint = self.endpoint.ok_or("wire: missing finite utterance endpoint")?;
        let first = self.first_pcm.ok_or("wire: missing reply PCM")?;
        first
            .checked_sub(endpoint)
            .ok_or_else(|| "wire: reply precedes utterance endpoint".into())
    }
}

/// Validate retained observations too: missing counters never count as silence
/// or success, and byte-level host evidence cannot substitute for signed PCM.
pub fn audible_measurement(measured: &serde_json::Value) -> Result<(), String> {
    audible_pcm(measured)?;
    let host = &measured["host"];
    if host["pcm_bytes"] != 9600 || host["nonzero_bytes"].as_u64().is_none_or(|n| !(1..=9600).contains(&n)) {
        return Err(format!(
            "wire: expected audible 9600-byte host playback; observed {host}"
        ));
    }
    Ok(())
}

/// Validate signed samples at the emitted reply boundary, independently of the sink.
pub fn audible_pcm(measured: &serde_json::Value) -> Result<(), String> {
    let nonzero = measured["nonzero_samples"].as_u64();
    let audible = measured["audible_samples"].as_u64();
    if measured["pcm_bytes"] != 9600
        || nonzero.is_none_or(|n| !(2400..=4800).contains(&n))
        || audible.is_none_or(|n| !(2400..=4800).contains(&n))
        || audible > nonzero
    {
        return Err(format!(
            "wire: expected audible 9600-byte reply with >=2400 signed samples; observed {measured}"
        ));
    }
    Ok(())
}

fn pcm(event: &Event, rate: u32) -> bool {
    matches!(event.typed(), Ok(WyomingEvent::AudioChunk(format))
        if format.rate == rate && format.width == 2 && format.channels == 1)
        && !event.payload.is_empty()
        && event.payload.len().is_multiple_of(2)
}

/// Nearest-rank p95 of actual current-case samples; never invent a missing one.
pub fn p95(samples: &[Duration]) -> Result<Duration, String> {
    if samples.is_empty() {
        return Err("wire: no latency samples".into());
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    // ceil(n * 95 / 100) == n - floor(n / 20), without multiplication overflow.
    Ok(ordered[ordered.len() - ordered.len() / 20 - 1])
}

pub struct Relay {
    pub address: SocketAddr,
    observed: watch::Receiver<Timing>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), String>>,
    restart: Option<oneshot::Sender<()>>,
}

impl Relay {
    // capture_bytes is the known finite fixture endpoint including its lead-in.
    // The fixture owner must supply it; this observer never infers speech/VAD.
    #[cfg(test)]
    pub async fn bind(host: SocketAddr, capture_bytes: usize) -> Result<Self, String> {
        if host.ip() != std::net::Ipv4Addr::LOCALHOST || host.port() == 0 {
            return Err("wire: host must be bound on 127.0.0.1".into());
        }
        if capture_bytes == 0 || capture_bytes > 65536 || !capture_bytes.is_multiple_of(2) {
            return Err("wire: invalid finite utterance size".into());
        }
        Self::start(host, Endpoint::Count(capture_bytes), false).await
    }

    pub async fn bind_fixture(host: SocketAddr, fixture: Vec<u8>) -> Result<Self, String> {
        Self::bind_session(host, fixture, false).await
    }

    pub async fn bind_session(host: SocketAddr, fixture: Vec<u8>, reconnect: bool) -> Result<Self, String> {
        if host.ip() != std::net::Ipv4Addr::LOCALHOST || host.port() == 0 {
            return Err("wire: host must be bound on 127.0.0.1".into());
        }
        if fixture.is_empty() || fixture.len() > 4096 || !fixture.len().is_multiple_of(2) {
            return Err("wire: invalid finite fixture".into());
        }
        Self::start(
            host,
            Endpoint::Fixture {
                bytes: signs(&fixture),
                tail: vec![],
            },
            reconnect,
        )
        .await
    }

    pub async fn bind_reply(host: SocketAddr) -> Result<Self, String> {
        if host.ip() != std::net::Ipv4Addr::LOCALHOST || host.port() == 0 {
            return Err("wire: host must be bound on 127.0.0.1".into());
        }
        Self::start(host, Endpoint::Reply, false).await
    }

    async fn start(host: SocketAddr, endpoint: Endpoint, reconnect: bool) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "wire: bind failed")?;
        let address = listener.local_addr().map_err(|_| "wire: address failed")?;
        let (observed, receive) = watch::channel(Timing::default());
        let (stop, stopped) = oneshot::channel();
        let (restart, restarted) = oneshot::channel();
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                result = reconnect::serve(listener, host, endpoint, &observed, reconnect.then_some(restarted)) => result,
                _ = stopped => Ok(()),
            };
            if let Err(error) = &result {
                observed.send_modify(|state| state.failure = Some(error.clone()));
            }
            result
        });
        Ok(Self {
            address,
            observed: receive,
            stop: Some(stop),
            task,
            restart: reconnect.then_some(restart),
        })
    }

    pub fn timing(&self) -> Timing {
        self.observed.borrow().clone()
    }

    #[cfg(test)]
    pub async fn wait_for_pcm(&self, minimum: usize) -> Result<(), String> {
        let mut observed = self.observed.clone();
        loop {
            let timing = observed.borrow_and_update().clone();
            if let Some(error) = timing.failure {
                return Err(error);
            }
            if timing.played >= minimum {
                return Ok(());
            }
            observed
                .changed()
                .await
                .map_err(|_| "wire: observer ended before PCM")?;
        }
    }

    pub async fn close(mut self) -> Result<Timing, String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task).await.map_err(|_| "cleanup: wire task failed")??;
        Ok(self.timing())
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    listener: &TcpListener,
    host: SocketAddr,
    endpoint: &mut Endpoint,
    observed: &watch::Sender<Timing>,
) -> Result<(), String> {
    let (bridge, _) = listener.accept().await.map_err(|_| "wire: accept failed")?;
    observed.send_modify(|state| state.session.generation());
    let host = TcpStream::connect(host)
        .await
        .map_err(|_| "wire: host connect failed")?;
    let (mut bridge_in, mut bridge_out) = bridge.into_split();
    let (mut host_in, mut host_out) = host.into_split();
    let epoch = Instant::now();
    tokio::select! {
        result = forward(&mut host_in, &mut bridge_out, observed, |state, event| {
            endpoint.observe(state, event, epoch.elapsed())
        }) => result,
        result = forward(&mut bridge_in, &mut host_out, observed, |state, event| {
            state.playback(event, epoch.elapsed())
        }) => result,
    }
}

enum Endpoint {
    Reply,
    #[cfg(test)]
    Count(usize),
    Fixture {
        bytes: Vec<u8>,
        tail: Vec<u8>,
    },
}

impl Endpoint {
    fn observe(&mut self, state: &mut Timing, event: &Event, now: Duration) -> Result<(), String> {
        match self {
            Self::Reply => state.capture(event, usize::MAX, now),
            #[cfg(test)]
            Self::Count(target) => state.capture(event, *target, now),
            Self::Fixture { bytes, tail } => {
                // Retain only a possible cross-frame match; never retain room PCM.
                state.capture(event, usize::MAX, now)?;
                if event.header.r#type == "audio-chunk" && state.run && !state.endpoint_seen() {
                    // Upstream microphone gain changes magnitudes but preserves
                    // signs. Match the complete finite fixture across frames.
                    tail.extend(signs(&event.payload));
                    if let Some(position) = tail.windows(bytes.len()).position(|window| window == bytes) {
                        state.endpoint = Some(now);
                        state.endpoint_sample = Some(state.captured / 2 - tail.len() + position + bytes.len());
                        tail.clear();
                    } else {
                        let discard = tail.len().saturating_sub(bytes.len() - 1);
                        tail.drain(..discard);
                    }
                }
                Ok(())
            }
        }
    }
}

fn signs(pcm: &[u8]) -> Vec<u8> {
    pcm.as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| {
            let sample = i16::from_le_bytes([bytes[0], bytes[1]]);
            u8::from(sample > 0) + 2 * u8::from(sample < 0)
        })
        .collect()
}

async fn forward(
    input: &mut (impl AsyncRead + Unpin),
    output: &mut (impl AsyncWrite + Unpin),
    observed: &watch::Sender<Timing>,
    mut observe: impl FnMut(&mut Timing, &Event) -> Result<(), String>,
) -> Result<(), String> {
    while let Some((bytes, event)) = frame(input).await? {
        output.write_all(&bytes).await.map_err(|_| "wire: forwarding failed")?;
        let mut result = Ok(());
        observed.send_modify(|state| result = observe(state, &event));
        result?;
    }
    Ok(())
}

pub(in crate::lab_run::sim) async fn frame(
    input: &mut (impl AsyncRead + Unpin),
) -> Result<Option<(Vec<u8>, Event)>, String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        match input.read(&mut byte).await {
            Ok(0) if bytes.is_empty() => return Ok(None),
            Err(error)
                if bytes.is_empty()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                    ) =>
            {
                return Ok(None);
            }
            Ok(0) | Err(_) => return Err("wire: incomplete header".into()),
            Ok(_) => bytes.push(byte[0]),
        }
        if bytes.len() > 4096 {
            return Err("wire: oversized header".into());
        }
        if byte[0] == b'\n' {
            break;
        }
    }
    let header: Header = serde_json::from_slice(&bytes).map_err(|_| "wire: malformed header")?;
    let length = header
        .data_length
        .checked_add(header.payload_length)
        .filter(|n| *n <= 65536)
        .ok_or("wire: oversized frame")?;
    let offset = bytes.len();
    bytes.resize(offset + length, 0);
    input
        .read_exact(&mut bytes[offset..])
        .await
        .map_err(|_| "wire: incomplete frame")?;
    let mut decoder = Decoder::new();
    let event = decoder
        .feed(&bytes)
        .map_err(|_| "wire: malformed frame")?
        .pop()
        .ok_or("wire: missing frame")?;
    decoder.finish().map_err(|_| "wire: incomplete frame")?;
    Ok(Some((bytes, event)))
}

#[cfg(test)]
mod tests;
