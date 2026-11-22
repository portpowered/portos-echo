use echo_contracts::wyoming::{Decoder, Event};
use serde_json::json;
use std::{
    fs::{self, File, OpenOptions},
    net::{Ipv4Addr, TcpListener},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::Command,
    time::timeout,
};

const BRIDGE: &str = env!("CARGO_BIN_EXE_echo-bridge");
const NAME: &str = "Lab Sim hassmic-name";
const CEILING: Duration = Duration::from_secs(8);

#[tokio::test]
#[ignore = "T3 exercises the prebuilt Linux satellite over loopback"]
async fn t3_hassmic_name_supervisor_cli() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let hassmic = copy_bridge(root, "hassmic");
    let _capture = keep_fifo_open(&root.join("capture"));
    let _keys = keep_fifo_open(&root.join("keys"));
    fs::create_dir(root.join("state")).unwrap();
    fs::write(root.join("state/config"), "even_volume=off\n").unwrap();
    fs::write(root.join("addrs"), "127.0.0.1\n").unwrap();

    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    let args = [
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
    ];
    let mut satellite = command(&hassmic, root);
    satellite.args(args).stderr(Stdio::piped());
    drop(reservation);
    let mut child = satellite.spawn().unwrap();
    let observed = timeout(CEILING, async {
        let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
        let mut startup = Vec::new();
        let marker = format!("wyoming on {port},");
        while let Some(line) = stderr.next_line().await.unwrap() {
            if line.contains(&marker) {
                let comm = fs::read_to_string(format!("/proc/{}/comm", child.id().unwrap())).unwrap();
                return Ok((comm, describe(port).await));
            }
            startup.push(line);
        }
        Err(startup)
    })
    .await;
    child.kill().await.unwrap();
    let (comm, info) = observed
        .expect("hassmic readiness and describe deadline")
        .expect("hassmic Wyoming readiness");
    assert_eq!(comm, "hassmic\n");
    assert_info(&info);
    println!(
        "T3-01 comm={:?} info={} payload_bytes={}",
        comm.trim_end(),
        info.data.unwrap(),
        info.payload.len()
    );
}

fn copy_bridge(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    fs::copy(BRIDGE, &path).unwrap();
    let metadata = path.symlink_metadata().unwrap();
    assert!(metadata.is_file());
    assert_ne!(metadata.permissions().mode() & 0o111, 0);
    path
}

fn keep_fifo_open(path: &Path) -> File {
    use rustix::fs::{CWD, Mode, mkfifoat};
    mkfifoat(CWD, path, Mode::RUSR | Mode::WUSR).unwrap();
    OpenOptions::new().read(true).write(true).open(path).unwrap()
}

fn command(binary: &Path, root: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .kill_on_drop(true)
        .current_dir(root)
        .env_clear()
        .env("HASSMIC_STATE", root.join("state"))
        .env("HASSMIC_CAP", root.join("capture"))
        .env("HASSMIC_PLAY", root.join("play.raw"))
        .env("HASSMIC_FAKE_LED", root.join("led"))
        .env("HASSMIC_FAKE_ADDRS", root.join("addrs"))
        .env("HASSMIC_MDNS_FILE", root.join("no-mdns"));
    command
}

async fn describe(port: u16) -> Event {
    let mut peer = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
    peer.write_all(b"{\"type\":\"describe\"}\n").await.unwrap();
    let mut decoder = Decoder::new();
    let mut bytes = [0; 1024];
    loop {
        let n = peer.read(&mut bytes).await.unwrap();
        assert_ne!(n, 0, "Wyoming EOF before info");
        for event in decoder.feed(&bytes[..n]).unwrap() {
            if event.header.r#type == "info" {
                return event;
            }
            assert_eq!(event.header.r#type, "hassmic-speaker");
        }
    }
}

fn assert_info(info: &Event) {
    let version = include_str!("../../../../third_party/hassmic/version").trim();
    let expected = json!({
        "name": NAME,
        "attribution": {"name": "hassmic", "url": ""},
        "installed": true,
        "description": "Echo Show 8 2nd gen (athena) with Amazon audio front end",
        "version": version,
        "area": null,
        "has_vad": false,
        "active_wake_words": ["alexa"],
        "max_active_wake_words": 1,
        "supports_trigger": true
    });
    assert_eq!(info.data.as_ref().unwrap()["satellite"], expected);
    assert_eq!(info.payload, b"");
}
