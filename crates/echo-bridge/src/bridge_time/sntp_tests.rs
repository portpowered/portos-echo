//! T0 packet and agreement rules; T3 exchanges with fake SNTP servers on 127.0.0.1.
use super::sntp::{self, Outcome, Sample};
use std::time::Duration;

/// 17.5 h: what the Echo was behind by when this was found.
pub(super) const BEHIND_MS: i64 = 63_000_000;
const TIMEOUT: Duration = Duration::from_millis(400);

#[derive(Clone, Copy)]
pub(super) struct Fake {
    pub offset_ms: i64,
    pub first: u8,
    pub stratum: u8,
}

impl Fake {
    pub(super) fn ahead(offset_ms: i64) -> Self {
        Self {
            offset_ms,
            first: 0x24,
            stratum: 2,
        }
    }

    /// Answers every request from its own clock moved by `offset_ms`; returns `127.0.0.1:PORT`.
    pub(super) async fn serve(self) -> String {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let name = socket.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let mut request = [0; 48];
            while let Ok((48, peer)) = socket.recv_from(&mut request).await {
                let origin = u64::from_be_bytes(request[40..48].try_into().unwrap());
                let now = sntp::stamp(sntp::system_ms() + self.offset_ms);
                let reply = reply(self.first, self.stratum, origin, now, now);
                socket.send_to(&reply, peer).await.unwrap();
            }
        });
        name
    }
}

fn reply(first: u8, stratum: u8, origin: u64, receive: u64, transmit: u64) -> [u8; 48] {
    let mut reply = [0; 48];
    reply[0] = first;
    reply[1] = stratum;
    reply[24..32].copy_from_slice(&origin.to_be_bytes());
    reply[32..40].copy_from_slice(&receive.to_be_bytes());
    reply[40..48].copy_from_slice(&transmit.to_be_bytes());
    reply
}

async fn refused() -> String {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.local_addr().unwrap().to_string()
}

#[test]
fn sntp_stamp_round_trips_through_the_2036_era_change() {
    for unix_ms in [
        0,
        1_795_152_420_123,
        2_085_978_496_000,
        2_085_978_497_250,
        4_000_000_000_999,
    ] {
        assert_eq!(sntp::unix_ms(sntp::stamp(unix_ms)), unix_ms);
    }
    assert_eq!(sntp::stamp(0) >> 32, 2_208_988_800);
}

#[test]
fn sntp_request_is_a_version_4_client_packet_carrying_only_its_transmit_stamp() {
    let request = sntp::request(0x0102_0304_0506_0708);
    assert_eq!(request[0], 0x23);
    assert!(request[1..40].iter().all(|byte| *byte == 0));
    assert_eq!(request[40..48], [1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn sntp_reply_gives_the_offset_and_the_round_trip_delay() {
    // Sent at 1000, the server (17.5 h ahead) holds it 10 ms, received 50 ms after sending.
    let origin = sntp::stamp(1000);
    let reply = reply(
        0x24,
        2,
        origin,
        sntp::stamp(1020 + BEHIND_MS),
        sntp::stamp(1030 + BEHIND_MS),
    );
    assert_eq!(
        sntp::parse(&reply, origin, 1000, 1050),
        Some(Sample {
            offset_ms: BEHIND_MS,
            delay_ms: 40
        })
    );
}

#[test]
fn sntp_reply_with_a_leap_stratum_mode_or_origin_problem_is_ignored() {
    let origin = sntp::stamp(1000);
    let now = sntp::stamp(1020);
    assert!(sntp::parse(&reply(0x24, 2, origin, now, now), origin, 1000, 1050).is_some());
    for (name, bytes) in [
        ("leap alarm: unsynchronized", reply(0xe4, 2, origin, now, now)),
        ("stratum 0: kiss of death", reply(0x24, 0, origin, now, now)),
        ("stratum 16: unsynchronized", reply(0x24, 16, origin, now, now)),
        ("mode 3: not a server", reply(0x23, 2, origin, now, now)),
        ("version 0", reply(0x04, 2, origin, now, now)),
        ("another request's reply", reply(0x24, 2, origin + 1, now, now)),
        ("no transmit stamp", reply(0x24, 2, origin, now, 0)),
        ("no receive stamp", reply(0x24, 2, origin, 0, now)),
    ] {
        assert_eq!(sntp::parse(&bytes, origin, 1000, 1050), None, "{name}");
    }
    assert_eq!(
        sntp::parse(&reply(0x24, 2, origin, now, now)[..47], origin, 1000, 1050),
        None
    );
    // The server claims to have held the request longer than the whole exchange took.
    let late = sntp::stamp(1520);
    assert_eq!(
        sntp::parse(&reply(0x24, 2, origin, now, late), origin, 1000, 1050),
        None
    );
}

#[test]
fn sntp_agreement_drops_the_outlier_and_needs_a_majority_and_the_quorum() {
    assert_eq!(sntp::agree(&[5000, 5040, 900_000], 2), Some((5020, 2)));
    assert_eq!(sntp::agree(&[900_000, 5040, 5000, 4960], 2), Some((5000, 3)));
    assert_eq!(sntp::agree(&[-3, 5, 1], 2), Some((1, 3)));
    assert_eq!(sntp::agree(&[5000, 900_000], 2), None);
    assert_eq!(sntp::agree(&[0, 5000, 900_000, -70_000], 2), None);
    assert_eq!(sntp::agree(&[5000], 2), None);
    assert_eq!(sntp::agree(&[5000], 1), Some((5000, 1)));
    assert_eq!(sntp::agree(&[], 1), None);
}

fn synced(outcome: Outcome) -> Option<(i64, usize)> {
    match outcome {
        Outcome::Synced { offset_ms, hosts } => Some((offset_ms, hosts)),
        Outcome::Unsynced { .. } => None,
    }
}

fn near(offset_ms: i64, expected: i64) -> bool {
    (offset_ms - expected).abs() < 250
}

#[tokio::test]
async fn t3_sntp_three_loopback_servers_give_the_known_offset() {
    let mut servers = vec![];
    for _ in 0..3 {
        servers.push(Fake::ahead(BEHIND_MS).serve().await);
    }
    let (offset_ms, hosts) = synced(sntp::measure(&servers, TIMEOUT).await).unwrap();
    assert!(near(offset_ms, BEHIND_MS), "{offset_ms}");
    assert_eq!(hosts, 3);
}

#[tokio::test]
async fn t3_sntp_outlier_and_bad_servers_do_not_move_the_offset() {
    let servers = vec![
        Fake::ahead(-5000).serve().await,
        Fake::ahead(900_000).serve().await,
        Fake::ahead(-5000).serve().await,
        Fake {
            stratum: 0,
            ..Fake::ahead(900_000)
        }
        .serve()
        .await,
        Fake {
            first: 0xe4,
            ..Fake::ahead(900_000)
        }
        .serve()
        .await,
        refused().await,
    ];
    let (offset_ms, hosts) = synced(sntp::measure(&servers, TIMEOUT).await).unwrap();
    assert!(near(offset_ms, -5000), "{offset_ms}");
    assert_eq!(hosts, 2);
}

#[tokio::test]
async fn t3_sntp_unreachable_disagreeing_or_absent_servers_are_unsynced() {
    let unreachable = vec![refused().await, refused().await];
    assert_eq!(
        sntp::measure(&unreachable, TIMEOUT).await,
        Outcome::Unsynced {
            reason: "unreachable",
            answered: 0
        }
    );
    let split = vec![Fake::ahead(0).serve().await, Fake::ahead(900_000).serve().await];
    assert_eq!(
        sntp::measure(&split, TIMEOUT).await,
        Outcome::Unsynced {
            reason: "disagreement",
            answered: 2
        }
    );
    let alone = vec![Fake::ahead(900_000).serve().await, refused().await];
    assert_eq!(
        sntp::measure(&alone, TIMEOUT).await,
        Outcome::Unsynced {
            reason: "disagreement",
            answered: 1
        }
    );
    assert_eq!(
        sntp::measure(&[], TIMEOUT).await,
        Outcome::Unsynced {
            reason: "disabled",
            answered: 0
        }
    );
}
