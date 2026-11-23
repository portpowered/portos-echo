//! T3: the device role over its Unix socket while the client stops reading
//! for 4 s in the middle of a 60 s reply. The capture thread sends microphone
//! audio under the core lock, so a send blocked on the small Unix buffer used
//! to stop the reader too: the reply audio and the cut waited behind it, and
//! after 5 s the send timed out and the device dropped the link.
use super::*;
use echo_contracts::wyoming::{Header, encode};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf},
    task::JoinHandle,
    time::Instant,
};

/// 60 s of 24 kHz reply in 20 ms chunks, offered at four times real time.
const CHUNKS: u32 = 3000;
const SPACING: Duration = Duration::from_millis(5);
/// The client stops reading from this chunk on, for about 4 s.
const STALL: std::ops::Range<u32> = 600..1250;

fn frame(kind: &str, data: Option<Value>, payload: Vec<u8>) -> Vec<u8> {
    encode(&Event {
        header: Header {
            r#type: kind.into(),
            data,
            data_length: 0,
            payload_length: 0,
        },
        data: None,
        payload,
    })
}

/// What the client read: arrival in ms since the reply began, the device's
/// capture timestamp, and the event type.
#[derive(Default)]
struct Seen {
    rows: Vec<(i64, Option<i64>, String)>,
    eof: bool,
}

impl Seen {
    fn first(&self, kind: &str, after: i64) -> Option<i64> {
        self.rows
            .iter()
            .find(|row| row.2 == kind && row.0 >= after)
            .map(|row| row.0)
    }
}

struct Pushed {
    written: u32,
    blocked: Duration,
    stalled: i64,
    resumed: i64,
}

fn millis(start: Instant) -> i64 {
    i64::try_from(start.elapsed().as_millis()).unwrap()
}

async fn handshake(peer: &mut Peer<UnixStream>, satellite: &fixture::Satellite) {
    peer.next("hassmic-speaker").await;
    peer.send("describe").await;
    peer.next("info").await;
    let failsafe = frame("run-satellite", Some(json!({"hassmic_failsafe_seconds":605})), vec![]);
    peer.io.write_all(&failsafe).await.unwrap();
    satellite.signal("-USR1").await;
    peer.next("run-pipeline").await;
}

fn reader(
    mut half: ReadHalf<UnixStream>,
    mut decoder: Decoder,
    start: Instant,
    paused: Arc<AtomicBool>,
    seen: Arc<Mutex<Seen>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut bytes = [0; 4096];
        loop {
            while paused.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let count = match timeout(Duration::from_millis(20), half.read(&mut bytes)).await {
                Err(_) => continue,
                Ok(Ok(0) | Err(_)) => break,
                Ok(Ok(count)) => count,
            };
            let at = millis(start);
            for event in decoder.feed(&bytes[..count]).unwrap() {
                let stamp = event.data.as_ref().and_then(|data| data["timestamp"].as_i64());
                seen.lock().unwrap().rows.push((at, stamp, event.header.r#type));
            }
        }
        seen.lock().unwrap().eof = true;
    })
}

async fn reply(writer: &mut WriteHalf<UnixStream>, paused: &AtomicBool, start: Instant) -> Pushed {
    let format = json!({"rate":24000,"width":2,"channels":1});
    writer
        .write_all(&frame("audio-start", Some(format.clone()), vec![]))
        .await
        .unwrap();
    let mut pushed = Pushed {
        written: 0,
        blocked: Duration::ZERO,
        stalled: 0,
        resumed: 0,
    };
    for index in 0..CHUNKS {
        if index == STALL.start {
            paused.store(true, Ordering::SeqCst);
            pushed.stalled = millis(start);
        }
        if index == STALL.end {
            paused.store(false, Ordering::SeqCst);
            pushed.resumed = millis(start);
        }
        let began = Instant::now();
        let chunk = frame("audio-chunk", Some(format.clone()), vec![0; 960]);
        let sent = timeout(Duration::from_secs(10), writer.write_all(&chunk)).await;
        pushed.blocked = pushed.blocked.max(began.elapsed());
        if !matches!(sent, Ok(Ok(()))) {
            break;
        }
        pushed.written += 1;
        tokio::time::sleep(SPACING).await;
    }
    pushed
}

/// Microphone audio after the stall: (chunks the client got late, the
/// largest lag of audio captured from 500 ms after the client read again).
fn lateness(seen: &Seen, resumed: i64) -> (usize, i64) {
    let audio: Vec<_> = seen
        .rows
        .iter()
        .filter(|row| row.2 == "audio-chunk")
        .filter_map(|row| row.1.map(|stamp| (row.0, stamp)))
        .collect();
    // The capture clock on the client's: the smallest offset seen before the stall.
    let offset = audio.iter().map(|(at, stamp)| at - stamp).min().unwrap();
    let late = audio.iter().filter(|(at, stamp)| at - stamp - offset > 1000).count();
    let lag = audio
        .iter()
        .filter(|(_, stamp)| stamp + offset >= resumed + 500)
        .map(|(at, stamp)| at - stamp - offset)
        .max()
        .unwrap_or(i64::MAX);
    (late, lag)
}

fn dropped_span(logs: &[String]) -> Option<i64> {
    let line = logs
        .iter()
        .find(|line| line.contains("mic: the client caught up, dropped "))?;
    line.split(" over ").nth(1)?.strip_suffix(" ms")?.parse().ok()
}

async fn exercise(setup: &Fixture, satellite: &fixture::Satellite) -> (Pushed, Arc<Mutex<Seen>>, i64) {
    let mut peer = Peer::new(UnixStream::connect(&setup.socket).await.unwrap());
    handshake(&mut peer, satellite).await;
    let Peer { io, decoder, .. } = peer;
    let (half, mut writer) = tokio::io::split(io);
    let (start, paused, seen) = (Instant::now(), Arc::new(AtomicBool::new(false)), Arc::default());
    let read = reader(half, decoder, start, paused.clone(), Arc::clone(&seen));
    let pushed = reply(&mut writer, &paused, start).await;
    paused.store(false, Ordering::SeqCst);
    // The cut: the client talks over the reply, the device stops and acknowledges.
    let cut = millis(start);
    let _ = writer.write_all(&frame("hassmic-flush", None, vec![])).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let _ = writer.write_all(&frame("ping", None, vec![])).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    read.abort();
    (pushed, seen, cut)
}

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_unix_reader_stall_keeps_reply_cut_and_link() {
    isolated("stall::t3_a1_unix_reader_stall_keeps_reply_cut_and_link", async {
        let setup = Fixture::new();
        let mut satellite = setup.start().await;
        let (pushed, seen, cut) = exercise(&setup, &satellite).await;
        satellite.signal("-TERM").await;
        let exit = timeout(LIMIT, satellite.child.wait()).await.unwrap().unwrap();
        let logs = satellite.rest().await;
        let seen = seen.lock().unwrap();
        let (late, lag) = lateness(&seen, pushed.resumed);
        let played = seen.first("played", cut).map(|at| at - cut);
        let pong = seen.first("pong", cut).is_some();
        let stall = pushed.resumed - pushed.stalled;
        writeln!(
            io::stdout(),
            "STALL written={} write_blocked_max_ms={} stall_ms={stall} late_mic_chunks={late} lag_after_ms={lag} played_after_cut_ms={played:?} pong={pong} eof={} dropped_span_ms={:?}",
            pushed.written,
            pushed.blocked.as_millis(),
            seen.eof,
            dropped_span(&logs),
        )
        .unwrap();
        assert_eq!(pushed.written, CHUNKS, "the device reads the whole reply: {logs:?}");
        assert!(pushed.blocked < Duration::from_secs(1), "reply writes wait {:?}", pushed.blocked);
        assert!(late <= 20, "{late} microphone chunks delivered over 1 s late");
        assert!(lag <= 250, "microphone audio {lag} ms late after the stall");
        assert!(played.is_some_and(|ms| ms <= 500), "the cut is acknowledged: {played:?}");
        assert!(pong && !seen.eof, "the link stays up");
        // The device drops microphone audio for about the stall, once, and only
        // sees the client leave when the test closes it.
        let span = dropped_span(&logs);
        assert!(span.is_some_and(|ms| ms >= 2500 && ms <= stall + 500), "{span:?} of {stall}: {logs:?}");
        let disconnects = logs.iter().filter(|line| line.contains("client disconnected")).count();
        assert_eq!(disconnects, 1, "{logs:?}");
        assert_eq!(exit.code(), Some(0));
    })
    .await;
}

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_unix_client_that_stops_reading_is_dropped() {
    isolated("stall::t3_a1_unix_client_that_stops_reading_is_dropped", async {
        let setup = Fixture::new();
        let mut satellite = setup.start().await;
        let mut silent = Peer::new(UnixStream::connect(&setup.socket).await.unwrap());
        handshake(&mut silent, &satellite).await;
        let start = Instant::now();
        // Nothing reads from here on: the device gives up as on a timed-out send.
        let found = timeout(Duration::from_secs(12), async {
            while let Some(line) = satellite.logs.next_line().await.unwrap() {
                if line.contains("mic: the client read nothing for 5 s, link dropped") {
                    return true;
                }
            }
            false
        })
        .await;
        let given_up = start.elapsed();
        assert_eq!(found.ok(), Some(true), "after {given_up:?}");
        satellite.wait_for_log("client disconnected").await;
        let mut replacement = Peer::new(UnixStream::connect(&setup.socket).await.unwrap());
        replacement.next("hassmic-speaker").await;
        writeln!(io::stdout(), "SILENT given_up_ms={}", given_up.as_millis()).unwrap();
        assert!((5000..7000).contains(&given_up.as_millis()), "{given_up:?}");
        satellite.signal("-TERM").await;
        let exit = timeout(LIMIT, satellite.child.wait()).await.unwrap().unwrap();
        assert_eq!(exit.code(), Some(0));
    })
    .await;
}
