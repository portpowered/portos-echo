//! T3 against a prebuilt echo-bridge copied as hassmic in an isolated namespace.

#![cfg(target_os = "linux")]

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::sync_channel,
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
    isolated("t3_initial_speaker_state", |satellite| {
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
    isolated("t3_speaker_setters", |satellite| {
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
    isolated("t3_wake_reply", |satellite| {
        let mut peer = satellite.connect();
        peer.arm();
        satellite.signal("USR1");
        peer.expect("run-pipeline", START);
        peer.read().assert_capture();
        peer.set("transcript", r#"{"text":"synthetic words"}"#, false);
        peer.set("synthesize", r#"{"text":"synthetic reply"}"#, false);
        peer.set("audio-start", r#"{"rate":16000,"width":2,"channels":1}"#, false);
        let pcm = [0x15, 0x02].repeat(640);
        peer.send(
            &format!("{{\"type\":\"audio-chunk\",\"payload_length\":{}}}\n", pcm.len()),
            &pcm,
        );
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
    isolated("t3_action_button_run", |satellite| {
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
    isolated("t3_reconnect_after_frame_failure", |satellite| {
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
        let mut abandoned = satellite.connect();
        abandoned.arm();
        satellite.signal("USR1");
        abandoned.expect("run-pipeline", START);
        abandoned.read().assert_capture();
        abandoned.set("audio-start", r#"{"rate":16000,"width":2,"channels":1}"#, false);
        let pcm = [0x15, 0x02].repeat(16000);
        abandoned.send("{\"type\":\"audio-chunk\",\"payload_length\":32000}\n", &pcm);
        abandoned.command("audio-stop");
        let mut byte = [0];
        assert_eq!(abandoned.0.get_ref().peek(&mut byte).unwrap(), 1);
        drop(abandoned);
        let mut peer = satellite.connect();
        peer.speaker(40, false);
        peer.command("describe");
        peer.info();
        peer.command("ping");
        peer.expect("pong", "");
    });
}

fn isolated(test: &str, run: impl FnOnce(&Satellite)) {
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
        run(&Satellite::start());
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
}

impl Satellite {
    fn start() -> Self {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let bridge = env::var_os("ECHO_DEVICE_TEST_BRIDGE")
            .map_or_else(|| workspace.join("target/debug/echo-bridge"), PathBuf::from);
        let dir = TempDir::new();
        let root = &dir.0;
        let alias = root.join("hassmic");
        fs::copy(&bridge, &alias).unwrap_or_else(|error| panic!("prebuilt {}: {error}", bridge.display()));
        fs::write(root.join("capture"), [0x0a, 0x02, 0xf6, 0xfd].repeat(400)).unwrap();
        fs::write(root.join("addrs"), "127.0.0.1\n").unwrap();
        fs::create_dir(root.join("state")).unwrap();
        fs::write(root.join("state/config"), "even_volume=off\n").unwrap();
        fs::create_dir(root.join("mixer")).unwrap();
        fs::write(root.join("mixer/MainVolume"), "40\n").unwrap();
        let keys = root.join("keys");
        assert!(Command::new("mkfifo").arg(&keys).status().unwrap().success());
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
        let child = command.spawn().unwrap();
        let mut satellite = Self {
            child,
            port,
            dir,
            _keys: key_handle,
        };
        satellite.ready();
        satellite
    }

    fn ready(&mut self) {
        let stderr = self.child.stderr.take().unwrap();
        let marker = format!("wyoming on {}, wake=local", self.port);
        let (ready, signal) = sync_channel(1);
        thread::spawn(move || {
            let mut ready = Some(ready);
            let mut startup = Vec::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(signal) = ready.take_if(|_| line.contains(&marker)) {
                    let _ = signal.send(Ok(()));
                } else if ready.is_some() {
                    startup.push(line);
                }
            }
            if let Some(signal) = ready {
                let _ = signal.send(Err(startup));
            }
        });
        signal
            .recv_timeout(TIMEOUT)
            .expect("satellite readiness deadline")
            .expect("satellite startup logs");
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

impl Drop for Satellite {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Peer(BufReader<TcpStream>);

impl Peer {
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
