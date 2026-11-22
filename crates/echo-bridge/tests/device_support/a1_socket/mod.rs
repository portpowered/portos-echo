//! T3 through the shipped bridge CLI and its copied hassmic role.

use super::*;
use echo_contracts::wyoming::{Decoder, Event};
use std::{
    collections::VecDeque,
    fs,
    future::Future,
    io,
    os::unix::fs::{MetadataExt, symlink},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream, UnixListener, UnixStream},
    time::timeout,
};

mod fixture;
use fixture::{Fixture, NAME, TCP, isolated, listener_counts, reject_terminal_components};

const LIMIT: Duration = Duration::from_secs(6);

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_socket_unix_audio_and_precedence() {
    isolated("t3_a1_socket_unix_audio_and_precedence", async {
        let mut setup = Fixture::new();
        let mut satellite = setup.start().await;
        assert_eq!(listener_counts(&setup.socket), (1, 0));
        let tcp = TcpListener::bind(TCP).await.unwrap();
        let live = bridge_peer::Live::bind_frames(0).await;
        let mut bridge = setup.bridge(&live.url);
        let pid = bridge.child.id().unwrap();
        assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
        wait_trace(&setup.trace, "wyoming.probe_ok", 1).await;
        satellite.signal("-USR1").await;
        wait_trace(&setup.trace, "mic.frames", 1).await;
        timeout(LIMIT, async {
            while live.microphone_frames.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert_eq!(live.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_no_tcp(&tcp).await;
        bridge.finish(0).await;
        satellite.wait_for_log("client disconnected").await;
        observe_wire(&mut setup, &satellite).await;
        writeln!(io::stdout(), "Unix bridge pid={pid} captured synthetic microphone audio, listeners before TCP fixture: Unix=1, TCP port 16700 count=0, TCP accepted=0").unwrap();
    }).await;
}

async fn observe_wire(setup: &mut Fixture, satellite: &fixture::Satellite) {
    let mut peer = Peer::new(UnixStream::connect(&setup.socket).await.unwrap());
    describe_and_register(&mut peer).await;
    satellite.signal("-USR1").await;
    let wake = peer.next("run-pipeline").await;
    assert_eq!(wake.data.unwrap()["wake_word_name"], "alexa");
    let (audio, sample) = capture(&mut peer).await;
    assert_capture_format(&audio);
    assert_mute(setup, &mut peer).await;
    writeln!(io::stdout(), "AF_UNIX speaker->info(name={NAME})->run-pipeline->audio-chunk header={:?} data={} s16le sample={sample:02x?}; muted=true then pong, zero intervening audio chunks", audio.header, audio.data.unwrap()).unwrap();
}

async fn describe_and_register(peer: &mut Peer<UnixStream>) {
    let speaker = peer.next("hassmic-speaker").await;
    assert_eq!(speaker.data, Some(json!({"volume":40,"muted":false})));
    peer.send("describe").await;
    let info = peer.next("info").await;
    assert_eq!(info.data.unwrap()["satellite"]["name"], NAME);
    peer.send("run-satellite").await;
    peer.send("ping").await;
    peer.next("pong").await;
}

fn assert_capture_format(audio: &Event) {
    let format = audio.data.as_ref().unwrap();
    assert_eq!(
        (
            format["rate"].as_u64(),
            format["width"].as_u64(),
            format["channels"].as_u64()
        ),
        (Some(16000), Some(2), Some(1))
    );
    assert_eq!(audio.header.payload_length, audio.payload.len());
    assert_eq!(audio.payload.len() % 2, 0);
}

async fn capture(peer: &mut Peer<UnixStream>) -> (Event, [u8; 4]) {
    // The existing wire starts with silent lead-in. The native gain stage
    // changes amplitude but preserves the fixture's alternating PCM polarity.
    timeout(LIMIT, async {
        loop {
            let audio = peer.next("audio-chunk").await;
            if let Some(sample) = alternating_samples(&audio.payload) {
                return (audio, sample);
            }
        }
    })
    .await
    .expect("capture contains the alternating PCM fixture")
}

async fn assert_mute(setup: &mut Fixture, peer: &mut Peer<UnixStream>) {
    setup.toggle_mic();
    let mute = timeout(LIMIT, async {
        loop {
            let event = peer.read().await;
            if event.header.r#type != "audio-chunk" {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(mute.header.r#type, "hassmic-mute");
    assert_eq!(mute.data, Some(json!({"muted":true})));
    peer.send("ping").await;
    peer.next("pong").await;
}

fn alternating_samples(bytes: &[u8]) -> Option<[u8; 4]> {
    bytes.as_chunks::<4>().0.iter().copied().find(|&[al, ah, bl, bh]| {
        let a = i16::from_le_bytes([al, ah]);
        let b = i16::from_le_bytes([bl, bh]);
        a.signum() * b.signum() == -1 && (i32::from(a) + i32::from(b)).abs() <= 4
    })
}

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_socket_absent_rolls_back_to_tcp() {
    isolated("t3_a1_socket_absent_rolls_back_to_tcp", async {
        let setup = Fixture::new();
        let tcp = TcpListener::bind(TCP).await.unwrap();
        let mut bridge = setup.bridge("http://127.0.0.1:1");
        let peer = tcp_describe(&tcp).await;
        assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
        assert_eq!(
            setup.socket.symlink_metadata().unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        writeln!(
            io::stdout(),
            "absent Unix path: TCP {TCP} completed describe/info name=Rollback-Sat, bridge.ready"
        )
        .unwrap();
        bridge.finish(0).await;
        drop(peer);
    })
    .await;
}

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_socket_refused_rolls_back_then_prefers_unix() {
    isolated("t3_a1_socket_refused_rolls_back_then_prefers_unix", async {
        let setup = Fixture::new();
        drop(UnixListener::bind(&setup.socket).unwrap());
        let stale = identity(&setup.socket);
        let tcp = TcpListener::bind(TCP).await.unwrap();
        let mut bridge = setup.bridge("http://127.0.0.1:1");
        let pid = bridge.child.id().unwrap();
        let peer = tcp_describe(&tcp).await;
        assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
        assert_eq!(identity(&setup.socket), stale, "the client leaves the refused inode in place");
        let mut satellite = setup.start().await;
        drop(peer);
        satellite.wait_for_log("client connected").await;
        wait_trace(&setup.trace, "wyoming.probe_ok", 1).await;
        assert_eq!(bridge.child.id(), Some(pid));
        assert_eq!(listener_counts(&setup.socket), (1, 1));
        assert_no_tcp(&tcp).await;
        writeln!(io::stdout(), "refused Unix inode {stale:?}: TCP describe/info name=Rollback-Sat; same bridge pid={pid} reconnected over Unix, TCP accepted=1").unwrap();
        bridge.finish(0).await;
    }).await;
}

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_socket_outage_recovers_without_restart() {
    isolated("t3_a1_socket_outage_recovers_without_restart", async {
        let setup = Fixture::new();
        let mut bridge = setup.bridge("http://127.0.0.1:1");
        let pid = bridge.child.id().unwrap();
        wait_trace(&setup.trace, "bridge.started", 1).await;
        let health = trace_or_empty(&setup.trace)[0]["health_addr"]
            .as_str()
            .unwrap()
            .to_owned();
        let response = super::health(&health, "GET", "/healthz").await;
        assert!(response.contains(r#""wyoming":"connecting""#));
        assert_eq!(listener_counts(&setup.socket), (0, 0));
        let mut satellite = setup.start().await;
        assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
        satellite.wait_for_log("client connected").await;
        assert_eq!(bridge.child.id(), Some(pid));
        assert_eq!(listener_counts(&setup.socket), (1, 0));
        writeln!(
            io::stdout(),
            "outage health=connecting, listeners=0; same bridge pid={pid} reached ready over Unix"
        )
        .unwrap();
        bridge.finish(0).await;
    })
    .await;
}

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_socket_duplicate_and_restart_lifecycle() {
    isolated("t3_a1_socket_duplicate_and_restart_lifecycle", async {
        let setup = Fixture::new();
        let mut satellite = setup.start().await;
        let live = bridge_peer::Live::bind_frames(0).await;
        let mut bridge = setup.bridge(&live.url);
        let pid = bridge.child.id().unwrap();
        assert_eq!(bridge.line().await, r#"{"event":"bridge.ready"}"#);
        let inode = identity(&setup.socket);
        let socket_mode = setup.socket.symlink_metadata().unwrap().mode() & 0o777;
        let directory_mode = setup.socket.parent().unwrap().symlink_metadata().unwrap().mode() & 0o777;
        assert_eq!((directory_mode, socket_mode), (0o750, 0o660));
        reject_endpoint(&setup, "Wyoming socket already owned").await;
        assert_eq!(identity(&setup.socket), inode);
        assert_eq!(listener_counts(&setup.socket), (1, 0));
        let probes = event_count(&setup.trace, "wyoming.probe_ok");
        wait_trace(&setup.trace, "wyoming.probe_ok", probes + 1).await;
        writeln!(io::stdout(), "duplicate exit=1 inode={inode:?} Unix listeners=1 TCP listeners=0 directory={directory_mode:04o} socket={socket_mode:04o}").unwrap();
        for (generation, signal) in ["-TERM", "-KILL"].into_iter().enumerate() {
            let old = identity(&setup.socket);
            satellite.signal(signal).await;
            let exit = timeout(LIMIT, satellite.child.wait()).await.unwrap().unwrap();
            assert_retired(&setup.socket, signal, exit, old);
            wait_trace(&setup.trace, "wyoming.reconnecting", generation + 1).await;
            satellite = setup.start().await;
            wait_trace(&setup.trace, "wyoming.connected", generation + 2).await;
            assert_eq!(bridge.child.id(), Some(pid));
            assert_eq!(listener_counts(&setup.socket), (1, 0));
            writeln!(io::stdout(), "{signal} old={old:?} replacement={:?} Unix listeners=1 TCP listeners=0 bridge pid={pid} probe generation={}", identity(&setup.socket), generation + 2).unwrap();
        }
        assert_eq!(event_count(&setup.trace, "wyoming.run_started"), 0);
        satellite.signal("-USR1").await;
        wait_trace(&setup.trace, "mic.frames", 1).await;
        assert_eq!(event_count(&setup.trace, "wyoming.run_started"), 1);
        bridge.finish(0).await;
    }).await;
}

fn assert_retired(path: &Path, signal: &str, exit: std::process::ExitStatus, old: (u64, u64)) {
    use std::os::unix::process::ExitStatusExt;
    if signal == "-TERM" {
        assert_eq!(exit.code(), Some(0));
        assert_eq!(path.symlink_metadata().unwrap_err().kind(), io::ErrorKind::NotFound);
    } else {
        assert_eq!(exit.signal(), Some(9));
        assert_eq!(identity(path), old);
        assert_eq!(
            std::os::unix::net::UnixStream::connect(path).unwrap_err().kind(),
            io::ErrorKind::ConnectionRefused
        );
    }
    assert_eq!(listener_counts(path), (0, 0));
}

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_socket_conflicts_leave_existing_entries() {
    isolated("t3_a1_socket_conflicts_leave_existing_entries", async {
        let setup = Fixture::new();
        fs::write(&setup.socket, b"preserve").unwrap();
        let old = identity(&setup.socket);
        reject_endpoint(&setup, "conflicting Wyoming endpoint entry").await;
        assert_eq!(identity(&setup.socket), old);
        assert_eq!(fs::read(&setup.socket).unwrap(), b"preserve");
        let target = setup.socket.with_extension("target");
        fs::rename(&setup.socket, &target).unwrap();
        symlink(&target, &setup.socket).unwrap();
        reject_endpoint(&setup, "conflicting Wyoming endpoint entry").await;
        assert_eq!(fs::read_link(&setup.socket).unwrap(), target);
        assert_eq!(identity(&target), old);
        fs::remove_file(&setup.socket).unwrap();
        let outside = UnixListener::bind(&setup.socket).unwrap();
        let external = identity(&setup.socket);
        reject_endpoint(&setup, "Wyoming listener is live").await;
        assert_eq!(identity(&setup.socket), external);
        assert_eq!(listener_counts(&setup.socket), (1, 0));
        drop(outside);
        fs::remove_file(&setup.socket).unwrap();
        assert_full_backlog_preserved(&setup).await;
        writeln!(
            io::stdout(),
            "file, symlink and external listener preserved; full Unix backlog refused reclamation; no TCP bind"
        )
        .unwrap();
    })
    .await;
}

async fn assert_full_backlog_preserved(setup: &Fixture) {
    let listener = tokio::net::UnixSocket::new_stream().unwrap();
    listener.bind(&setup.socket).unwrap();
    let _listener = listener.listen(0).unwrap();
    let _pending = timeout(LIMIT, UnixStream::connect(&setup.socket))
        .await
        .unwrap()
        .unwrap();
    let inode = identity(&setup.socket);
    reject_endpoint(setup, &io::Error::from(rustix::io::Errno::AGAIN).to_string()).await;
    assert_eq!(identity(&setup.socket), inode);
    assert_eq!(listener_counts(&setup.socket), (1, 0));
}

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_socket_unsafe_or_malformed_unix_does_not_select_tcp() {
    isolated("t3_a1_socket_unsafe_or_malformed_unix_does_not_select_tcp", async {
        let setup = Fixture::new();
        let tcp = TcpListener::bind(TCP).await.unwrap();
        symlink("absent", &setup.socket).unwrap();
        let mut bridge = setup.bridge("http://127.0.0.1:1");
        wait_trace(&setup.trace, "bridge.started", 1).await;
        assert_no_tcp(&tcp).await;
        fs::remove_file(&setup.socket).unwrap();
        let unix = UnixListener::bind(&setup.socket).unwrap();
        let mut peer = Peer::new(timeout(LIMIT, unix.accept()).await.unwrap().unwrap().0);
        peer.next("run-satellite").await;
        peer.next("describe").await;
        peer.io.write_all(b"malformed Wyoming\n").await.unwrap();
        bridge.collect(1).await;
        assert_no_tcp(&tcp).await;
        writeln!(
            io::stdout(),
            "dangling Unix symlink and malformed Wyoming each left TCP accepted=0"
        )
        .unwrap();
    })
    .await;
}

#[tokio::test]
#[ignore = "T3 uses prebuilt Linux bridge and a private loopback namespace"]
async fn t3_a1_socket_discovery_and_option_errors() {
    isolated("t3_a1_socket_discovery_and_option_errors", async {
        let setup = Fixture::new();
        let path = setup.socket.to_str().unwrap();
        let unix = setup.device().args(["-S", "--wyoming-socket", path]).output().await.unwrap();
        assert_eq!((unix.status.code(), unix.stdout, unix.stderr), (Some(0), vec![], vec![]));
        assert_eq!(fs::read_dir(setup.socket.parent().unwrap()).unwrap().count(), 0);
        assert_eq!(listener_counts(&setup.socket), (0, 0));
        let tcp = setup.device().arg("-S").output().await.unwrap();
        assert_eq!(tcp.status.code(), Some(0));
        assert_eq!(tcp.stderr, b"");
        assert_eq!(String::from_utf8(tcp.stdout).unwrap(), concat!(
            "<?xml version=\"1.0\" standalone='no'?>\n<!DOCTYPE service-group SYSTEM \"avahi-service.dtd\">\n",
            "<service-group>\n  <name replace-wildcards=\"yes\">Lab-Sat %h</name>\n",
            "  <service><type>_wyoming._tcp</type><port>16700</port></service>\n</service-group>\n"
        ));
        for args in [vec!["--wyoming-socket"], vec!["--wyoming-socket", "relative"], vec!["--wyoming-socket", path, "--wyoming-socket", path]] {
            let result = setup.device().args(args).output().await.unwrap();
            assert_eq!(result.status.code(), Some(2));
            assert_eq!(result.stdout, b"");
            assert!(String::from_utf8(result.stderr).unwrap().contains("--wyoming-socket"));
        }
        reject_terminal_components(&setup).await;
        assert_eq!(fs::read_dir(setup.socket.parent().unwrap()).unwrap().count(), 0);
        assert_eq!(listener_counts(&setup.socket), (0, 0));
        writeln!(io::stdout(), "-S Unix stdout=0 bytes; TCP discovery retains exact XML and port=16700; terminal dot paths exit=2 in device startup, discovery and bridge lab mode with zero endpoint entries and no trace").unwrap();
    }).await;
}

async fn reject_endpoint(setup: &Fixture, expected: &str) {
    let duplicate = timeout(
        LIMIT,
        setup.device().arg("--wyoming-socket").arg(&setup.socket).output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(String::from_utf8(duplicate.stderr).unwrap().contains(expected));
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = path.symlink_metadata().unwrap();
    (metadata.dev(), metadata.ino())
}

async fn tcp_describe(listener: &TcpListener) -> Peer<TcpStream> {
    let mut peer = Peer::new(timeout(LIMIT, listener.accept()).await.unwrap().unwrap().0);
    peer.next("run-satellite").await;
    peer.next("describe").await;
    peer.io
        .write_all(b"{\"type\":\"info\",\"data\":{\"satellite\":{\"name\":\"Rollback-Sat\"}}}\n")
        .await
        .unwrap();
    peer
}

async fn assert_no_tcp(listener: &TcpListener) {
    assert!(
        timeout(Duration::ZERO, listener.accept()).await.is_err(),
        "unexpected TCP fallback"
    );
}

fn trace_or_empty(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn event_count(path: &Path, event: &str) -> usize {
    trace_or_empty(path).iter().filter(|row| row["event"] == event).count()
}

async fn wait_trace(path: &Path, event: &str, count: usize) {
    let observed = timeout(LIMIT, async {
        while event_count(path, event) < count {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await;
    assert!(
        observed.is_ok(),
        "waiting for {count} {event} events, observed {}",
        event_count(path, event)
    );
}

struct Peer<T> {
    io: T,
    decoder: Decoder,
    backlog: VecDeque<Event>,
}

impl<T: AsyncRead + AsyncWrite + Unpin> Peer<T> {
    fn new(io: T) -> Self {
        Self {
            io,
            decoder: Decoder::new(),
            backlog: VecDeque::new(),
        }
    }

    async fn send(&mut self, kind: &str) {
        self.io
            .write_all(format!("{{\"type\":\"{kind}\"}}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn next(&mut self, kind: &str) -> Event {
        let event = self.read().await;
        assert_eq!(event.header.r#type, kind);
        event
    }

    async fn read(&mut self) -> Event {
        timeout(LIMIT, async {
            let mut bytes = [0; 2048];
            loop {
                if let Some(event) = self.backlog.pop_front() {
                    return event;
                }
                let count = self.io.read(&mut bytes).await.unwrap();
                assert_ne!(count, 0, "Wyoming EOF before frame");
                self.backlog.extend(self.decoder.feed(&bytes[..count]).unwrap());
            }
        })
        .await
        .unwrap()
    }
}
