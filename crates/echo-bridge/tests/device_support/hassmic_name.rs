use echo_contracts::wyoming::{Decoder, Event};
use serde_json::json;
use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    io,
    net::{Ipv4Addr, TcpListener},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Output, Stdio},
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
const USAGE: &str = "usage: hassmic [-P esphome|wyoming] [-p port] [-n name] [-w local|remote] [-m manifest] [-b input-device] [-z port] [-o port] [-a port] [-i port] [-W port] [-G dir] [-L] [-E] [-V] [-S] [-D]";

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
    describe_copy(&mut satellite, reservation, port).await;

    ring_output(&hassmic, root).await;
    avahi_output(&hassmic, root, &args, port, "T3-03 hassmic").await;
    let invalid = run(&hassmic, root, &["--invalid-test-option"]).await;
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(invalid.stdout, b"");
    let stderr = String::from_utf8(invalid.stderr).unwrap();
    assert_eq!(stderr.lines().last(), Some(USAGE));
    println!("T3-04 --invalid-test-option exit=2 stdout=\"\" stderr={stderr:?}");

    let host = copy_bridge(root, "hassmic-host");
    avahi_output(&host, root, &args, port, "T3-05 hassmic-host").await;
    let near = copy_bridge(root, "hassmic-extra");
    for binary in [&near, Path::new(BRIDGE)] {
        let help = run(binary, root, &["--help"]).await;
        assert_eq!(help.status.code(), Some(0));
        assert_eq!(help.stderr, b"");
        assert_eq!(
            help.stdout,
            format!("{}\n", echo_bridge::bridge_operator::HELP).as_bytes()
        );
        println!(
            "T3-05 {} --help exit=0 stderr=\"\" stdout:\n{}",
            binary.file_name().unwrap().display(),
            String::from_utf8(help.stdout).unwrap()
        );
    }
}

async fn describe_copy(command: &mut Command, reservation: TcpListener, port: u16) {
    drop(reservation);
    let mut child = command.spawn().unwrap();
    let pid = child.id().unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
    let observed = timeout(CEILING, async {
        let mut startup = Vec::new();
        let marker = format!("wyoming on {port},");
        while let Some(line) = stderr.next_line().await? {
            if line.contains(&marker) {
                let comm = fs::read_to_string(format!("/proc/{pid}/comm"))?;
                return Ok((comm, describe(port).await?));
            }
            startup.push(line);
        }
        Err(Box::<dyn Error>::from(format!(
            "stderr before Wyoming readiness: {startup:?}"
        )))
    })
    .await;
    let exit = child.try_wait().unwrap();
    if exit.is_none() {
        child.kill().await.unwrap();
    }
    let (comm, info) = observed
        .expect("T3-01 hassmic Wyoming readiness/describe deadline")
        .unwrap_or_else(|error| panic!("T3-01 hassmic Wyoming readiness or info failed: exit={exit:?}, {error}"));
    assert_eq!(comm, "hassmic\n");
    assert_info(&info);
    println!(
        "T3-01 comm={:?} info={} payload_bytes={}",
        comm.trim_end(),
        info.data.unwrap(),
        info.payload.len()
    );
}

async fn ring_output(hassmic: &Path, root: &Path) {
    fs::create_dir(root.join("ring")).unwrap();
    let diagnostic = "ring: no LED ring on this model (the band is drawn at run time)\n";
    for destination in ["-", "ring"] {
        let output = run(hassmic, root, &["-G", destination]).await;
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        assert_eq!(output.stderr, diagnostic.as_bytes());
        println!("T3-02 -G {destination} exit=1 stdout=\"\" stderr={diagnostic:?}");
    }
    assert_eq!(fs::read_dir(root.join("ring")).unwrap().count(), 0);
    println!("T3-02 ring directory entries=0");
}

async fn avahi_output(binary: &Path, root: &Path, args: &[&str], port: u16, label: &str) {
    let mut args = args.to_vec();
    args.push("-S");
    let output = run(binary, root, &args).await;
    let xml = String::from_utf8(output.stdout).unwrap();
    let expected = format!(
        "<?xml version=\"1.0\" standalone='no'?>\n\
        <!DOCTYPE service-group SYSTEM \"avahi-service.dtd\">\n\
        <service-group>\n  <name replace-wildcards=\"yes\">{NAME} %h</name>\n  \
        <service><type>_wyoming._tcp</type><port>{port}</port></service>\n</service-group>\n"
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stderr, b"");
    assert_eq!(xml, expected);
    println!("{label} -S exit=0 stderr=\"\" stdout:\n{xml}");
}

async fn run(binary: &Path, root: &Path, args: &[&str]) -> Output {
    timeout(CEILING, command(binary, root).args(args).output())
        .await
        .expect("one-shot CLI deadline")
        .unwrap()
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

async fn describe(port: u16) -> Result<Event, Box<dyn Error>> {
    let mut peer = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
    peer.write_all(b"{\"type\":\"describe\"}\n").await?;
    let mut decoder = Decoder::new();
    let mut bytes = [0; 1024];
    loop {
        let n = peer.read(&mut bytes).await?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Wyoming EOF before info").into());
        }
        for event in decoder.feed(&bytes[..n])? {
            match event.header.r#type.as_str() {
                "info" => return Ok(event),
                "hassmic-speaker" => (),
                kind => return Err(format!("expected Wyoming info, received {kind}").into()),
            }
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
