//! T3 against a prebuilt echo-bridge copied as hassmic in an isolated namespace.

#![cfg(target_os = "linux")]
#![cfg(test)]

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{Receiver, channel, sync_channel},
    thread,
    time::{Duration, Instant},
};

const NAME: &str = "Lab-Sat";
const NETNS: &str = "ECHO_DEVICE_T3_PARENT_NETNS";
const TIMEOUT: Duration = Duration::from_secs(5);
const VERSION: &str = include_str!("../../../third_party/hassmic/version");
const START: &str = r#"{"start_stage":"asr","end_stage":"tts","restart_on_end":false,"wake_word_name":"alexa"}"#;
const SPEAKER: &str = "hassmic-speaker";

#[test]
#[ignore = "T3 uses a prebuilt Linux bridge and a private loopback namespace"]
fn t3_initial_speaker_state() {
    isolated("t3_initial_speaker_state", || {
        let satellite = Satellite::start();
        let mut peer = satellite.connect();
        peer.command("describe");
        peer.command("ping");
        peer.speaker(40, false);
        peer.info();
        peer.expect("pong", "");
    });
}

#[test]
#[ignore = "T3 uses a prebuilt Linux bridge and a private loopback namespace"]
fn t3_speaker_setters() {
    isolated("t3_speaker_setters", || {
        let satellite = Satellite::start();
        let mut peer = satellite.connect();
        peer.speaker(40, false);
        for (volume, inline) in [(0, true), (100, false), (100, true)] {
            peer.set("hassmic-set-volume", &format!("{{\"volume\":{volume}}}"), inline);
            peer.speaker(volume, false);
        }
        for (mute, inline) in [(true, false), (true, true), (false, false)] {
            peer.set("hassmic-set-output-mute", &format!("{{\"mute\":{mute}}}"), inline);
            peer.speaker(100, mute);
        }
        peer.set("hassmic-set-volume", r#"{"volume":101}"#, false);
        peer.set("hassmic-set-output-mute", r#"{"mute":1}"#, true);
        peer.command("ping");
        peer.expect("pong", "");
        drop(peer);
        satellite.connect().speaker(100, false);
    });
}

#[test]
#[ignore = "T3 uses a prebuilt Linux bridge and a private loopback namespace"]
fn t3_wake_reply() {
    isolated("t3_wake_reply", || {
        let satellite = Satellite::start();
        let mut peer = satellite.connect();
        peer.arm();
        satellite.signal("USR1");
        peer.expect("run-pipeline", START);
        peer.read().assert_capture();
        peer.set("transcript", r#"{"text":"synthetic words"}"#, false);
        peer.set("synthesize", r#"{"text":"synthetic reply"}"#, false);
        peer.set("audio-start", r#"{"rate":16000,"width":2,"channels":1}"#, false);
        peer.audio_chunk(&[0x15, 0x02].repeat(640));
        peer.command("audio-stop");
        peer.expect_after_capture("played");
        assert_eq!(fs::metadata(satellite.dir.0.join("play.raw")).unwrap().len(), 1280);
        peer.read().assert_capture();
        peer.command("ping");
        peer.expect_after_capture("pong");
    });
}

#[test]
#[ignore = "T3 uses a prebuilt Linux bridge and a private loopback namespace"]
fn t3_action_button_run() {
    isolated("t3_action_button_run", || {
        let satellite = Satellite::start();
        let mut peer = satellite.connect();
        peer.arm();
        satellite.signal("USR2");
        peer.expect("run-pipeline", START);
        peer.read().assert_capture();
    });
}

#[test]
#[ignore = "T3 uses a prebuilt Linux bridge and a private loopback namespace"]
fn t3_reconnect_after_frame_failure() {
    isolated("t3_reconnect_after_frame_failure", || {
        let satellite = Satellite::start();
        drop(satellite.connect());
        for broken in [
            "{\"type\":\"ping\",\"data_length\":20}\n{}",
            "{\"type\":\"audio-chunk\",\"payload_length\":1048577}\n",
            "{\"type\":\"ping\",\"data_length\":8192}\n",
        ] {
            let mut peer = satellite.connect();
            peer.speaker(40, false);
            peer.send(broken, &[]);
            peer.0.get_ref().shutdown(Shutdown::Write).unwrap();
            assert_eq!(peer.0.read(&mut [0; 1]).unwrap(), 0, "{broken:?}");
        }
        satellite.connect().expect_fresh_session();
    });
}

#[test]
#[ignore = "T3 uses a prebuilt Linux bridge and a private loopback namespace"]
fn t3_reconnect_after_write_failure() {
    isolated("t3_reconnect_after_write_failure", || {
        // A small MTU keeps Linux's MSS-based minimum buffers small enough
        // for capture to fill them in this private namespace.
        let mtu = Command::new("ip").args(["link", "set", "lo", "mtu", "1500"]).status();
        assert!(mtu.unwrap().success(), "set private loopback MTU");
        let settings = [
            "-qw",
            "net.ipv4.tcp_wmem=4096 4096 4096",
            "net.ipv4.tcp_rmem=4096 4096 4096",
        ];
        let buffers = Command::new("sysctl").args(settings).status();
        assert!(buffers.unwrap().success(), "set private TCP buffer sizes");
        let satellite = Satellite::start();
        let mut blocked = satellite.connect();
        blocked.arm();
        satellite.signal("USR1");
        blocked.expect("run-pipeline", START);
        blocked.read().assert_capture();
        let mut replacement = satellite.connect();
        // The old peer stays open and unread through the native send timeout.
        let deadline = Instant::now() + Duration::from_secs(12);
        satellite.wait_for_log("client disconnected", deadline);
        replacement.expect_fresh_session();
        assert!(Instant::now() < deadline, "replacement handshake exceeded 12 seconds");
        assert!(io::copy(&mut blocked.0, &mut io::sink()).unwrap() > 0);
    });
}

#[test]
#[ignore = "T3 uses a prebuilt Linux bridge and a private loopback namespace"]
fn t3_reconnect_fences_old_playback() {
    isolated("t3_reconnect_fences_old_playback", || {
        let satellite = Satellite::start();
        let output = satellite.dir.0.join("play.raw");
        make_fifo(&output);
        let playback = OpenOptions::new().read(true).write(true).open(output).unwrap();
        let mut old = satellite.connect();
        old.speaker(40, false);
        old.set("audio-start", r#"{"rate":192000,"width":2,"channels":16}"#, false);
        // Keep the worker in one native sink write larger than the FIFO. It
        // cannot process the queued flush while the test holds the read end.
        old.audio_chunk(&[0x15, 0x02].repeat(60_000));
        let mut tap = playback.try_clone().unwrap();
        let (sample, observed) = sync_channel(1);
        thread::spawn(move || {
            let mut first = [0; 2];
            let _ = sample.send(tap.read_exact(&mut first).map(|()| first));
        });
        assert_eq!(observed.recv_timeout(TIMEOUT).unwrap().unwrap(), [0x15, 0x02]);
        old.command("hassmic-flush");
        old.command("ping");
        old.expect("pong", "");
        drop(old);

        let mut replacement = satellite.connect();
        replacement.expect_fresh_session();
        drop(playback);
        // The worker logs completion under core_lock immediately before the
        // played callback. The following pong is serialized behind both.
        satellite.wait_for_log("duplex flush: receipt_ms=", Instant::now() + TIMEOUT);
        replacement.command("ping");
        replacement.expect("pong", "");
    });
}

fn isolated(test: &str, run: impl FnOnce()) {
    let current = fs::read_link("/proc/self/ns/net").unwrap();
    if let Some(parent) = env::var_os(NETNS) {
        assert_ne!(current.as_os_str(), parent);
        let devices = fs::read_to_string("/proc/self/net/dev").unwrap();
        let interfaces: Vec<_> = devices
            .lines()
            .skip(2)
            .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim()))
            .collect();
        assert_eq!(interfaces, ["lo"]);
        run();
        return;
    }
    let status = Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--net",
            "--",
            "/bin/sh",
            "-c",
            "ip link set lo up && exec \"$@\"",
            "sh",
        ])
        .arg(env::current_exe().unwrap())
        .args(["--exact", test, "--ignored", "--nocapture"])
        .env(NETNS, current)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "{test} inside the loopback namespace exited with {status}"
    );
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let output = Command::new("mktemp")
            .args(["-d", "-t", "echo-satellite.XXXXXXXX"])
            .output()
            .unwrap();
        assert!(output.status.success());
        Self(String::from_utf8(output.stdout).unwrap().trim_end().into())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Satellite {
    child: Child,
    port: u16,
    dir: TempDir,
    _keys: File,
    logs: Receiver<String>,
}

impl Satellite {
    fn start() -> Self {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let bridge = env::var_os("ECHO_DEVICE_TEST_BRIDGE")
            .map_or_else(|| workspace.join("target/debug/echo-bridge"), PathBuf::from);
        let dir = TempDir::new();
        let root = &dir.0;
        let alias = root.join("hassmic");
        fs::copy(&bridge, &alias)
            .map_err(|error| format!("prebuilt {}: {error}", bridge.display()))
            .expect("copy the prebuilt bridge ELF");
        fs::write(root.join("capture"), [0x0a, 0x02, 0xf6, 0xfd].repeat(400)).unwrap();
        fs::write(root.join("addrs"), "127.0.0.1\n").unwrap();
        fs::create_dir(root.join("state")).unwrap();
        fs::write(root.join("state/config"), "even_volume=off\n").unwrap();
        fs::create_dir(root.join("mixer")).unwrap();
        fs::write(root.join("mixer/MainVolume"), "40\n").unwrap();
        let keys = root.join("keys");
        make_fifo(&keys);
        let key_handle = OpenOptions::new().read(true).write(true).open(&keys).unwrap();
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        let mut command = Command::new(alias);
        command
            .current_dir(root)
            .env_clear()
            .env("HASSMIC_STATE", root.join("state"))
            .env("HASSMIC_CAP", root.join("capture"))
            .env("HASSMIC_PLAY", root.join("play.raw"))
            .env("HASSMIC_FAKE_MIXER", root.join("mixer"))
            .env("HASSMIC_FAKE_LED", root.join("led"))
            .env("HASSMIC_FAKE_ADDRS", root.join("addrs"))
            .env("HASSMIC_MDNS_FILE", root.join("no-mdns"))
            .args([
                "-P",
                "wyoming",
                "-D",
                "-p",
                &port.to_string(),
                "-b",
                "keys",
                "-n",
                NAME,
                "-a",
                "0",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        drop(reservation);
        let mut child = command.spawn().unwrap();
        let logs = satellite_logs(&mut child);
        let satellite = Self {
            child,
            port,
            dir,
            _keys: key_handle,
            logs,
        };
        satellite.wait_for_log(&format!("wyoming on {port}, wake=local"), Instant::now() + TIMEOUT);
        satellite
    }

    fn wait_for_log(&self, marker: &str, deadline: Instant) {
        let mut seen = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self
                .logs
                .recv_timeout(remaining)
                .map_err(|error| format!("waiting for {marker:?}: {error}; satellite logs: {seen:?}"))
                .expect("satellite log checkpoint");
            if line.contains(marker) {
                return;
            }
            seen.push(line);
        }
    }

    fn connect(&self) -> Peer {
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port)).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(TIMEOUT)).unwrap();
        Peer(BufReader::new(stream))
    }

    fn signal(&self, signal: &str) {
        assert!(
            Command::new("kill")
                .args([&format!("-{signal}"), &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
    }
}

fn satellite_logs(child: &mut Child) -> Receiver<String> {
    let stderr = child.stderr.take().unwrap();
    let (lines, logs) = channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = lines.send(line);
        }
    });
    logs
}

fn make_fifo(path: &Path) {
    assert!(Command::new("mkfifo").arg(path).status().unwrap().success());
}

impl Drop for Satellite {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Peer(BufReader<TcpStream>);

impl Peer {
    fn expect_fresh_session(&mut self) {
        self.speaker(40, false);
        self.command("describe");
        self.info();
        self.command("ping");
        self.expect("pong", "");
    }

    fn command(&mut self, kind: &str) {
        self.send(&format!("{{\"type\":\"{kind}\"}}\n"), &[]);
    }

    fn set(&mut self, kind: &str, data: &str, inline: bool) {
        if inline {
            self.send(&format!("{{\"type\":\"{kind}\",\"data\":{data}}}\n"), &[]);
        } else {
            self.send(
                &format!("{{\"type\":\"{kind}\",\"data_length\":{}}}\n", data.len()),
                data.as_bytes(),
            );
        }
    }

    fn send(&mut self, header: &str, body: &[u8]) {
        let stream = self.0.get_mut();
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    }

    fn audio_chunk(&mut self, pcm: &[u8]) {
        self.send(
            &format!("{{\"type\":\"audio-chunk\",\"payload_length\":{}}}\n", pcm.len()),
            pcm,
        );
    }

    fn arm(&mut self) {
        self.speaker(40, false);
        self.command("run-satellite");
        self.command("ping");
        self.expect("pong", "");
    }

    fn read(&mut self) -> Frame {
        let mut header = String::new();
        assert_ne!(self.0.read_line(&mut header).unwrap(), 0, "waiting for Wyoming frame");
        let (kind, lengths) = header
            .strip_prefix("{\"type\":\"")
            .unwrap()
            .split_once("\",\"version\":\"1.5.4\"")
            .unwrap();
        let mut frame = Frame {
            kind: kind.into(),
            data: String::new(),
            payload: vec![0; length(lengths, "payload_length")],
        };
        let mut bytes = vec![0; length(lengths, "data_length")];
        self.0.read_exact(&mut bytes).unwrap();
        frame.data = String::from_utf8(bytes).unwrap();
        self.0.read_exact(&mut frame.payload).unwrap();
        frame
    }

    fn expect(&mut self, kind: &str, data: &str) {
        self.read().assert_event(kind, data);
    }

    fn expect_after_capture(&mut self, kind: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "waiting for {kind} during capture");
            let frame = self.read();
            if frame.kind == kind {
                frame.assert_event(kind, "");
                break;
            }
            frame.assert_capture();
        }
    }

    fn speaker(&mut self, volume: i32, muted: bool) {
        self.expect(SPEAKER, &format!(r#"{{"volume":{volume},"muted":{muted}}}"#));
    }

    fn info(&mut self) {
        let version = VERSION.trim();
        let expected = format!(
            concat!(
                r#"{{"asr":[],"tts":[],"handle":[],"intent":[],"wake":[],"mic":[],"snd":[],"satellite":{{"name":"{}","#,
                r#""attribution":{{"name":"hassmic","url":""}},"installed":true,"description":"Echo Show 8 2nd gen (athena) with Amazon audio front end","#,
                r#""version":"{}","area":null,"has_vad":false,"active_wake_words":["alexa"],"max_active_wake_words":1,"supports_trigger":true}}}}"#
            ),
            NAME, version
        );
        self.expect("info", &expected);
    }
}

struct Frame {
    kind: String,
    data: String,
    payload: Vec<u8>,
}

impl Frame {
    fn assert_event(&self, kind: &str, data: &str) {
        assert_eq!(self.kind, kind, "unexpected Wyoming frame with data {}", self.data);
        assert_eq!(self.data, data, "unexpected Wyoming {kind} data");
        assert_eq!(self.payload, b"", "unexpected Wyoming {kind} payload");
    }

    fn assert_capture(&self) {
        assert_eq!(self.kind, "audio-chunk");
        let timestamp = self
            .data
            .strip_prefix(r#"{"rate":16000,"width":2,"channels":1,"timestamp":"#)
            .and_then(|value| value.strip_suffix('}'))
            .unwrap();
        assert!(timestamp.parse::<u64>().is_ok());
        assert!((2..=2048).contains(&self.payload.len()) && self.payload.len().is_multiple_of(2));
    }
}

fn length(header: &str, key: &str) -> usize {
    header
        .split_once(&format!(",\"{key}\":"))
        .map_or(0, |(_, rest)| rest.split([',', '}']).next().unwrap().parse().unwrap())
}
