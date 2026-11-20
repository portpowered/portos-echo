//! A small SNTP (RFC 4330) client: one question per server, then agreement.
//!
//! A reply counts only from a synchronized server (leap indicator not 3,
//! stratum 1 to 15, server mode) that echoes this request's transmit stamp.
//! The answers that lie within [`TOLERANCE_MS`] of their median must be a
//! majority and at least two (one where a single server is configured);
//! their mean is the offset. Anything else is unsynced and changes nothing.
//! No address, name or packet body leaves this module.
use std::{
    net::SocketAddr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::net::UdpSocket;

const NTP_UNIX_SECONDS: i64 = 2_208_988_800;
const ERA_SECONDS: i64 = 1 << 32;
const NTP_PORT: u16 = 123;
const ATTEMPTS: usize = 2;
/// Answers further than this from the median are outliers.
pub(crate) const TOLERANCE_MS: i64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sample {
    pub offset_ms: i64,
    pub delay_ms: i64,
}

/// `offset_ms` is network time minus the system clock: positive when the clock is behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Synced { offset_ms: i64, hosts: usize },
    Unsynced { reason: &'static str, answered: usize },
}

/// The system clock as milliseconds since the Unix epoch.
pub(crate) fn system_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

/// Unix milliseconds as a 32.32 NTP timestamp; the seconds wrap at each era.
pub(crate) fn stamp(unix_ms: i64) -> u64 {
    let seconds = (unix_ms.div_euclid(1000) + NTP_UNIX_SECONDS).rem_euclid(ERA_SECONDS);
    // Rounded up, so that reading the stamp back (rounded down) gives the same millisecond.
    let fraction = (u64::try_from(unix_ms.rem_euclid(1000)).unwrap_or_default() << 32).div_ceil(1000);
    (u64::try_from(seconds).unwrap_or_default() << 32) | fraction
}

/// A stamp read in the era that holds today: seconds below 2^31 are after 2036.
pub(crate) fn unix_ms(stamp: u64) -> i64 {
    let seconds = i64::try_from(stamp >> 32).unwrap_or_default();
    let fraction = i64::try_from(stamp & 0xffff_ffff).unwrap_or_default();
    let era = if seconds < ERA_SECONDS / 2 { ERA_SECONDS } else { 0 };
    (seconds + era - NTP_UNIX_SECONDS) * 1000 + ((fraction * 1000) >> 32)
}

/// Version 4, client mode; only the transmit stamp is set.
pub(crate) fn request(transmit: u64) -> [u8; 48] {
    let mut packet = [0; 48];
    packet[0] = 0x23;
    packet[40..48].copy_from_slice(&transmit.to_be_bytes());
    packet
}

fn field(reply: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(reply.get(at..at + 8)?.try_into().ok()?))
}

/// `sent_ms` and `received_ms` are this side's clock around the exchange.
pub(crate) fn parse(reply: &[u8], origin: u64, sent_ms: i64, received_ms: i64) -> Option<Sample> {
    let first = *reply.first()?;
    let stratum = *reply.get(1)?;
    let (leap, version, mode) = (first >> 6, (first >> 3) & 7, first & 7);
    let (receive, transmit) = (field(reply, 32)?, field(reply, 40)?);
    if leap == 3
        || !(1..=4).contains(&version)
        || mode != 4
        || !(1..=15).contains(&stratum)
        || field(reply, 24)? != origin
        || receive == 0
        || transmit == 0
    {
        return None;
    }
    let (receive, transmit) = (unix_ms(receive), unix_ms(transmit));
    let delay_ms = (received_ms - sent_ms) - (transmit - receive);
    if delay_ms < 0 || transmit < receive {
        return None;
    }
    Some(Sample {
        offset_ms: i64::midpoint(receive - sent_ms, transmit - received_ms),
        delay_ms,
    })
}

/// The mean of the answers near the median and how many they are.
pub(crate) fn agree(offsets: &[i64], quorum: usize) -> Option<(i64, usize)> {
    let mut sorted = offsets.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    let median = if sorted.len().is_multiple_of(2) {
        sorted.get(middle.checked_sub(1)?)?.checked_add(*sorted.get(middle)?)? / 2
    } else {
        *sorted.get(middle)?
    };
    let kept: Vec<i64> = sorted
        .into_iter()
        .filter(|offset| offset.abs_diff(median) <= TOLERANCE_MS.unsigned_abs())
        .collect();
    if kept.len() * 2 <= offsets.len() || kept.len() < quorum.max(1) {
        return None;
    }
    let sum: i128 = kept.iter().map(|offset| i128::from(*offset)).sum();
    let mean = sum / i128::try_from(kept.len()).ok()?;
    Some((i64::try_from(mean).ok()?, kept.len()))
}

/// Ask every server at once. `timeout` bounds each name lookup and each exchange.
pub(crate) async fn measure(servers: &[String], timeout: Duration) -> Outcome {
    if servers.is_empty() {
        return Outcome::Unsynced {
            reason: "disabled",
            answered: 0,
        };
    }
    let mut queries = tokio::task::JoinSet::new();
    for server in servers {
        let server = server.clone();
        queries.spawn(async move { query(&server, timeout).await });
    }
    let mut offsets = vec![];
    while let Some(result) = queries.join_next().await {
        if let Ok(Some(sample)) = result {
            offsets.push(sample.offset_ms);
        }
    }
    match agree(&offsets, servers.len().min(2)) {
        Some((offset_ms, hosts)) => Outcome::Synced { offset_ms, hosts },
        None => Outcome::Unsynced {
            reason: if offsets.is_empty() {
                "unreachable"
            } else {
                "disagreement"
            },
            answered: offsets.len(),
        },
    }
}

async fn query(server: &str, timeout: Duration) -> Option<Sample> {
    let name = if server.contains(':') {
        server.to_owned()
    } else {
        format!("{server}:{NTP_PORT}")
    };
    let found: Vec<SocketAddr> = tokio::time::timeout(timeout, tokio::net::lookup_host(name))
        .await
        .ok()?
        .ok()?
        .collect();
    let address = *found.iter().find(|address| address.is_ipv4()).or(found.first())?;
    let local: SocketAddr = if address.is_ipv4() {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = UdpSocket::bind(local).await.ok()?;
    socket.connect(address).await.ok()?;
    for _ in 0..ATTEMPTS {
        if let Ok(Some(sample)) = tokio::time::timeout(timeout, exchange(&socket)).await {
            return Some(sample);
        }
    }
    None
}

// The reply's arrival is timed on the monotonic clock: a clock set meanwhile cannot bend it.
async fn exchange(socket: &UdpSocket) -> Option<Sample> {
    let sent_ms = system_ms();
    let origin = stamp(sent_ms);
    let started = Instant::now();
    socket.send(&request(origin)).await.ok()?;
    let mut reply = [0; 128];
    loop {
        let length = socket.recv(&mut reply).await.ok()?;
        let elapsed = i64::try_from(started.elapsed().as_millis()).ok()?;
        // A late answer to an earlier attempt carries another origin: keep waiting.
        if let Some(sample) = parse(reply.get(..length)?, origin, sent_ms, sent_ms + elapsed) {
            return Some(sample);
        }
        if field(&reply, 24) == Some(origin) {
            return None;
        }
    }
}
