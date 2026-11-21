//! T3: paired Settings commit and replacement, then real pinned start requests.
use super::*;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
use tokio::process::{Child, ChildStdin, ChildStdout};

type Lines = tokio::io::Lines<BufReader<ChildStdout>>;
const SIM_EPOCH: &str = "1800000000";

#[tokio::test]
async fn t3_pinned_runner_fast_mode() {
    let cases = Box::pin(async { tokio::join!(exercise(true), exercise(false)) });
    let (on, off) = tokio::time::timeout(Duration::from_secs(25), cases).await.unwrap();
    let (priority, live_on) = compared_start(&on, "priority".into(), "priority");
    let (standard, live_off) = compared_start(&off, Value::Null, "default");
    assert_eq!(priority, standard);
    assert_eq!(live_on, live_off);
    writeln!(
        std::io::stdout(),
        "Paired on/off: each saved pending, reloaded, then returned no-op. Pinned runner accepts priority and present null (standard). Full Live prompts match at the fixed epoch."
    )
    .unwrap();
}

async fn exercise(fast: bool) -> Vec<Value> {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = configure(root, listener.local_addr().unwrap().port());
    let base = root.join("bridge.toml");
    let home = root.join("agent-home");
    let settings = home.join("settings.toml");
    let original = configure_fast_mode(&mut config, &base, &settings, fast);
    let (mut provider, origin, mut provider_output) = start_provider().await;
    save_and_reload(root, &listener, &config, &origin, fast).await;
    assert_persisted(&base, &settings, &original, fast);
    let mut provider_input = provider.stdin.take().unwrap();
    Box::pin(pinned_call(
        root,
        &listener,
        &config,
        &origin,
        fast,
        (&mut provider_input, &mut provider_output),
    ))
    .await;
    drop(provider_input);
    assert!(provider.wait().await.unwrap().success());
    drop(
        echo_codex::codex_supervisor::Supervisor::new(echo_codex::codex_supervisor::SupervisorConfig::new(
            config.binary.unwrap(),
            config.home.unwrap(),
        ))
        .unwrap(),
    );
    observations(root)
}

async fn start_provider() -> (Child, String, Lines) {
    let mut provider = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut provider_output = BufReader::new(provider.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&provider_output.next_line().await.unwrap().unwrap()).unwrap();
    (provider, ready["origin"].as_str().unwrap().to_owned(), provider_output)
}

async fn save_and_reload(root: &Path, listener: &TcpListener, config: &CodexConfig, origin: &str, fast: bool) {
    // A Settings change that requires real exec is refused in fixed-clock mode.
    let mut bridge = run_bridge(root, origin, false);
    let mut output = BufReader::new(bridge.stdout.take().unwrap()).lines();
    let satellite = listener.accept().await.unwrap().0;
    let replacement = replace(root, listener, satellite, &mut output, config, fast).await;
    drop(bridge.stdin.take());
    assert_clean_exit(bridge).await;
    drop(replacement);
    assert_eq!(count(root, "settings.restart_prepared"), 1);
    assert_eq!(count(root, "bridge.ready"), 2);
}

fn assert_persisted(base: &Path, settings: &Path, original: &str, fast: bool) {
    assert_eq!(fs::read(base).unwrap(), original.as_bytes());
    assert_eq!(fs::metadata(settings).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(
        fs::read_to_string(settings).unwrap(),
        format!("[codex]\ndelegate_fast = {fast}\n\n[voice]\nsession_listen_seconds = 4\n")
    );
}

async fn pinned_call(
    root: &Path,
    listener: &TcpListener,
    config: &CodexConfig,
    origin: &str,
    fast: bool,
    (provider_input, provider_output): (&mut ChildStdin, &mut Lines),
) {
    let mut bridge = run_bridge(root, origin, true);
    let mut input = bridge.stdin.take().unwrap();
    let mut output = BufReader::new(bridge.stdout.take().unwrap()).lines();
    let mut satellite = listener.accept().await.unwrap().0;
    admit(&mut satellite, &mut output, config).await;
    assert_snapshot(
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &settings_url(root),
        fast,
    )
    .await;
    let mut peer = connect_call(
        root,
        &mut satellite,
        (provider_input, provider_output),
        (&mut input, &mut output),
    )
    .await;
    drop(input);
    acknowledge_cut(&mut satellite).await;
    assert_clean_exit(bridge).await;
    peer.close();
}

async fn assert_clean_exit(bridge: Child) {
    let result = bridge.wait_with_output().await.unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
}

fn configure_fast_mode(config: &mut CodexConfig, base: &Path, settings: &Path, fast: bool) -> String {
    let root = base.parent().unwrap();
    let home = settings.parent().unwrap();
    fs::create_dir(home).unwrap();
    let observer = root.join("observe-stdio");
    let pin: PathBuf = config.binary.replace(observer.clone()).unwrap();
    let text = format!(
        "#!/usr/bin/python3\nPIN = {}\nCAPTURE = {}\n{}",
        json!(pin),
        json!(root.join("starts")),
        include_str!("observe_stdio.py"),
    );
    fs::write(&observer, text).unwrap();
    fs::set_permissions(&observer, fs::Permissions::from_mode(0o700)).unwrap();
    let text = fs::read_to_string(base).unwrap().replace(
        &format!("binary = '{}'", pin.display()),
        &format!("binary = '{}'", observer.display()),
    );
    let text = format!(
        "{text}\ndelegate_model = 'gpt-6-sol'\ndelegate_effort = 'low'\ndelegate_fast = true\n\n[home]\ndir = '{}'\nagent_tools = false\n",
        home.display()
    );
    fs::write(base, &text).unwrap();
    let initial = if fast { "[codex]\ndelegate_fast = false\n\n" } else { "" };
    fs::write(settings, format!("{initial}[voice]\nsession_listen_seconds = 4\n")).unwrap();
    text
}

fn run_bridge(root: &Path, origin: &str, fixed_clock: bool) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_echo-bridge"));
    command.current_dir(root).args([
        "--config",
        "bridge.toml",
        "--trace",
        "trace",
        "--health-listen",
        "127.0.0.1:0",
        "--live-loopback",
        origin,
        "--model-loopback",
        origin,
    ]);
    if fixed_clock {
        command.args(["--sim-clock", "--sim-epoch", SIM_EPOCH]);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

async fn replace(
    root: &Path,
    listener: &TcpListener,
    mut old: TcpStream,
    output: &mut Lines,
    config: &CodexConfig,
    fast: bool,
) -> TcpStream {
    admit(&mut old, output, config).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let before = settings_url(root);
    assert_snapshot(&client, &before, !fast).await;
    save(&client, &before, fast, true).await;
    assert_eq!(old.read_to_end(&mut vec![]).await.unwrap(), 0);
    let mut active = listener.accept().await.unwrap().0;
    admit(&mut active, output, config).await;
    let after = settings_url(root);
    assert_snapshot(&client, &after, fast).await;
    save(&client, &after, fast, false).await;
    active
}

fn settings_url(root: &Path) -> reqwest::Url {
    let address = lifecycle::screen_address(root);
    assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
    let mut url = reqwest::Url::parse("http://127.0.0.1/settings/v2?token=synthetic-screen-pair").unwrap();
    url.set_port(Some(address.port())).unwrap();
    url
}

async fn save(client: &reqwest::Client, url: &reqwest::Url, fast: bool, pending: bool) {
    let response = client
        .patch(url.clone())
        .json(&json!({"version":2,"settings":{"codex":{"delegate_fast":fast}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"version":2,"restart_pending":pending})
    );
}

async fn assert_snapshot(client: &reqwest::Client, url: &reqwest::Url, fast: bool) {
    let response = client.get(url.clone()).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let data: Value = response.json().await.unwrap();
    assert_eq!(data["settings"]["codex"]["delegate_fast"], fast);
    assert_eq!(data["settings"]["voice"]["session_listen_seconds"], 4);
    assert_eq!(data["restart_pending"], false);
}

async fn connect_call(
    root: &Path,
    satellite: &mut TcpStream,
    (provider_input, provider_output): (&mut ChildStdin, &mut Lines),
    (clock_input, clock_output): (&mut ChildStdin, &mut Lines),
) -> LivePeer {
    let mut screen = lifecycle::screen(root).await;
    wake(satellite).await;
    let request: Value = serde_json::from_str(&provider_output.next_line().await.unwrap().unwrap()).unwrap();
    let mut peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let answer = peer.answer(request["offer"].as_str().unwrap()).await.unwrap();
    provider_input
        .write_all(format!("{}\n", json!({"answer":answer})).as_bytes())
        .await
        .unwrap();
    let connected =
        async { while lifecycle::next_frame(&mut screen).await.unwrap()["state"]["state"] != "connected" {} };
    tokio::pin!(connected);
    loop {
        tokio::select! {
            () = &mut connected => break,
            () = step(&mut peer) => advance(clock_input, clock_output).await,
        }
    }
    peer
}

async fn acknowledge_cut(satellite: &mut TcpStream) {
    loop {
        let (header, payload) = read_wire(satellite).await;
        assert_eq!(payload, [] as [u8; 0]);
        if header["type"] == "hassmic-flush" {
            send(satellite, "played", json!({}), vec![]).await;
            return;
        }
        assert_eq!(header["type"], "audio-start");
    }
}

fn observations(root: &Path) -> Vec<Value> {
    fs::read_to_string(root.join("starts"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn compared_start(rows: &[Value], sent: Value, accepted: &str) -> (Value, Value) {
    let [runner, live]: &[Value; 2] = rows.try_into().expect("one runner and one Live start");
    let mut runner_start = runner["request"].clone();
    let mut live_start = live["request"].clone();
    assert_eq!(runner_start["method"], "thread/start");
    assert_eq!(live_start["method"], "thread/realtime/start");
    assert_eq!(runner_start["params"]["model"], "gpt-6-sol");
    assert_eq!(runner_start["params"]["config"]["model_reasoning_effort"], "low");
    assert_eq!(runner["reply"]["result"]["serviceTier"], accepted);
    assert_eq!(live["reply"]["result"], json!({}));
    let thread = runner["reply"]["result"]["thread"]["id"].as_str().unwrap();
    assert_ne!(thread, "");
    assert_eq!(live_start["params"]["threadId"], thread);
    for (row, request) in [(runner, &mut runner_start), (live, &mut live_start)] {
        assert_eq!(row["reply"]["id"], request["id"]);
        assert!(request.as_object_mut().unwrap().remove("id").unwrap().is_number());
    }
    assert_eq!(
        runner_start["params"].as_object_mut().unwrap().remove("serviceTier"),
        Some(sent)
    );
    live_start["params"]["threadId"] = "<generated thread>".into();
    assert_fixed_context(&runner_start, &live_start);
    let sdp = &mut live_start["params"]["transport"]["sdp"];
    *sdp = normalized_sdp(sdp.as_str().unwrap()).into();
    writeln!(
        std::io::stdout(),
        "Pinned thread/start accepted tier {accepted}; paired save and exactly one restart"
    )
    .unwrap();
    (runner_start, live_start)
}

fn assert_fixed_context(runner: &Value, live: &Value) {
    let context = "[Call-start context: 2027-01-15 Friday 08:00:00 UTC+00:00; local offset unavailable]"; // lint-rules: allow-literal UTC calendar expectation, never an endpoint
    let base = runner["params"]["baseInstructions"].as_str().unwrap();
    let prompt = live["params"]["prompt"].as_str().unwrap();
    assert_eq!(live["params"]["version"], "v3");
    assert_eq!(live["params"].get("voice"), None);
    assert_eq!(live["params"].get("model"), None);
    assert_eq!(base.lines().last(), Some(context));
    assert_eq!(prompt.lines().last(), Some(context));
    assert_eq!(live["params"]["initialItems"][0]["text"], prompt);
}

fn normalized_sdp(offer: &str) -> String {
    loopback_offer(offer);
    assert!(offer.lines().any(|line| line.starts_with("m=audio ")));
    assert!(offer.lines().any(|line| line.starts_with("m=application ")));
    let ssrc = offer
        .lines()
        .find_map(|line| line.strip_prefix("a=ssrc:")?.split_once(' ').map(|(id, _)| id))
        .unwrap();
    assert!(ssrc.parse::<u32>().is_ok());
    let track = format!("audio-{ssrc}");
    offer
        .lines()
        .map(|line| normalize_sdp_line(line, ssrc, &track))
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_sdp_line(line: &str, ssrc: &str, track: &str) -> String {
    if let Some(origin) = line.strip_prefix("o=") {
        let mut fields: Vec<_> = origin.split_whitespace().collect();
        assert!(fields[1].parse::<u64>().is_ok());
        assert!(fields[2].parse::<u64>().is_ok());
        fields[1] = "<session ID>";
        fields[2] = "<session version>";
        return format!("o={}", fields.join(" "));
    }
    let Some((key, value)) = line.split_once(':') else {
        return line.to_owned();
    };
    match key {
        "a=ice-ufrag" | "a=ice-pwd" => {
            assert_ne!(value, "");
            format!("{key}:<generated>")
        }
        "a=fingerprint" => {
            let (algorithm, fingerprint) = value.split_once(' ').unwrap();
            assert_ne!(fingerprint, "");
            format!("{key}:{algorithm} <generated>")
        }
        "a=candidate" => {
            let mut fields: Vec<_> = value.split_whitespace().collect();
            assert!(fields[5].parse::<u16>().unwrap() > 0);
            fields[5] = "<ephemeral port>";
            format!("{key}:{}", fields.join(" "))
        }
        "a=ssrc" => {
            let (id, attributes) = value.split_once(' ').unwrap();
            assert_eq!(id, ssrc);
            let attributes = attributes.replace(track, "audio-<generated>");
            format!("{key}:<generated> {attributes}")
        }
        "a=msid" => {
            assert_eq!(value, format!("realtime {track}"));
            format!("{key}:realtime audio-<generated>")
        }
        _ => line.to_owned(),
    }
}
