//! T3: paired Settings commit and replacement, then real pinned start requests.
use super::*;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

#[tokio::test]
async fn t3_pinned_runner_fast_mode() {
    let cases = async { tokio::join!(exercise(true), exercise(false)) };
    let (on, off) = tokio::time::timeout(Duration::from_secs(25), cases).await.unwrap();
    let (priority, live_on) = compared_start(on, "priority".into(), "priority");
    let (standard, live_off) = compared_start(off, Value::Null, "default");
    assert_eq!(priority, standard);
    assert_eq!(live_on, live_off);
    writeln!(
        std::io::stdout(),
        "Paired on/off: each saved pending, reloaded, then returned no-op. Pinned runner accepts priority and present null (standard). Runner and Live request comparisons match."
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
    let mut provider = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut provider_input = provider.stdin.take().unwrap();
    let mut provider_output = BufReader::new(provider.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&provider_output.next_line().await.unwrap().unwrap()).unwrap();
    let mut bridge = run_bridge(root, ready["origin"].as_str().unwrap());
    let stdin = bridge.stdin.take().unwrap();
    let mut output = BufReader::new(bridge.stdout.take().unwrap()).lines();
    let satellite = listener.accept().await.unwrap().0;
    let mut active = replace(root, &listener, satellite, &mut output, &config, fast).await;
    assert_eq!(fs::read(base).unwrap(), original.into_bytes());
    assert_eq!(fs::metadata(&settings).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(
        fs::read_to_string(settings).unwrap(),
        format!("[codex]\ndelegate_fast = {fast}\n\n[voice]\nsession_listen_seconds = 4\n")
    );
    let mut peer = connect_call(root, &mut active, &mut provider_input, &mut provider_output).await;
    drop(stdin);
    acknowledge_cut(&mut active).await;
    let result = bridge.wait_with_output().await.unwrap();
    peer.close();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(count(root, "settings.restart_prepared"), 1);
    assert_eq!(count(root, "bridge.ready"), 2);
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

fn run_bridge(root: &Path, origin: &str) -> tokio::process::Child {
    Command::new(env!("CARGO_BIN_EXE_echo-bridge"))
        .current_dir(root)
        .args([
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
        ])
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
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
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

fn settings_url(root: &Path) -> String {
    format!(
        "http://{}/settings/v2?token=synthetic-screen-pair",
        lifecycle::screen_address(root)
    )
}

async fn save(client: &reqwest::Client, url: &str, fast: bool, pending: bool) {
    let response = client
        .patch(url)
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

async fn assert_snapshot(client: &reqwest::Client, url: &str, fast: bool) {
    let response = client.get(url).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let data: Value = response.json().await.unwrap();
    assert_eq!(data["settings"]["codex"]["delegate_fast"], fast);
    assert_eq!(data["settings"]["voice"]["session_listen_seconds"], 4);
    assert_eq!(data["restart_pending"], false);
}

async fn connect_call(
    root: &Path,
    satellite: &mut TcpStream,
    input: &mut tokio::process::ChildStdin,
    output: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> LivePeer {
    let mut screen = lifecycle::screen(root).await;
    wake(satellite).await;
    let request: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let mut peer = LivePeer::bind(([127, 0, 0, 1], 0).into(), Instant::now().into_std())
        .await
        .unwrap();
    let answer = peer.answer(request["offer"].as_str().unwrap()).await.unwrap();
    input
        .write_all(format!("{}\n", json!({"answer":answer})).as_bytes())
        .await
        .unwrap();
    let connected =
        async { while lifecycle::next_frame(&mut screen).await.unwrap()["state"]["state"] != "connected" {} };
    tokio::pin!(connected);
    loop {
        tokio::select! {
            () = &mut connected => break,
            () = step(&mut peer) => (),
        }
    }
    peer
}

async fn acknowledge_cut(satellite: &mut TcpStream) {
    loop {
        let (header, payload) = read_wire(satellite).await;
        match header["type"].as_str().unwrap() {
            "audio-start" => assert!(payload.is_empty()),
            "hassmic-flush" => {
                assert!(payload.is_empty());
                send(satellite, "played", json!({}), vec![]).await;
                return;
            }
            unexpected => panic!("unexpected Wyoming frame before shutdown flush: {unexpected}"),
        }
    }
}

fn observations(root: &Path) -> Vec<Value> {
    fs::read_to_string(root.join("starts"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn compared_start(rows: Vec<Value>, sent: Value, accepted: &str) -> (Value, Value) {
    let [runner, live] = rows.as_slice() else {
        panic!("expected one runner and one Live start");
    };
    let mut runner_start = runner["request"].clone();
    let mut live_start = live["request"].clone();
    assert_eq!(runner_start["method"], "thread/start");
    assert_eq!(live_start["method"], "thread/realtime/start");
    assert_eq!(runner_start["params"]["model"], "gpt-6-sol");
    assert_eq!(runner_start["params"]["config"]["model_reasoning_effort"], "low");
    assert_eq!(runner["reply"]["result"]["serviceTier"], accepted);
    assert_eq!(live["reply"]["result"], json!({}));
    let thread = runner["reply"]["result"]["thread"]["id"].as_str().unwrap();
    assert!(!thread.is_empty());
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
    normalize_context(&mut runner_start, &mut live_start);
    let sdp = &mut live_start["params"]["transport"]["sdp"];
    *sdp = normalized_sdp(sdp.as_str().unwrap()).into();
    writeln!(
        std::io::stdout(),
        "Pinned thread/start accepted tier {accepted}; paired save and exactly one restart"
    )
    .unwrap();
    (runner_start, live_start)
}

fn normalize_context(runner: &mut Value, live: &mut Value) {
    // The delivered binary appends its current wall-clock sample to both prompts.
    // Compare the sample within each generation, then compare all authored text
    // and context formatting across generations. The fixed-input T1 holds it exact.
    fn timestamp(prompt: &str) -> &str {
        let date = prompt.rsplit_once("\n[Call-start context: ").unwrap().1;
        date.split_once(" UTC+00:00; ").unwrap().0
    }
    let base = runner["params"]["baseInstructions"].as_str().unwrap();
    let prompt = live["params"]["prompt"].as_str().unwrap();
    let time = timestamp(base);
    assert_eq!(timestamp(prompt), time);
    assert_eq!(live["params"]["initialItems"][0]["text"], prompt);
    let (base, prompt) = (
        base.replacen(time, "<clock sample>", 1),
        prompt.replacen(time, "<clock sample>", 1),
    );
    runner["params"]["baseInstructions"] = base.into();
    live["params"]["prompt"] = prompt.clone().into();
    live["params"]["initialItems"][0]["text"] = prompt.into();
}

fn normalized_sdp(offer: &str) -> String {
    loopback_offer(offer);
    assert!(offer.lines().any(|line| line.starts_with("m=audio ")));
    assert!(offer.lines().any(|line| line.starts_with("m=application ")));
    offer
        .lines()
        .map(|line| {
            if let Some((key, value)) = line.split_once(':') {
                if matches!(key, "a=ice-ufrag" | "a=ice-pwd") {
                    assert!(!value.is_empty());
                    return format!("{key}:<generated>");
                }
                if key == "a=fingerprint" {
                    let (algorithm, fingerprint) = value.split_once(' ').unwrap();
                    assert!(!fingerprint.is_empty());
                    return format!("{key}:{algorithm} <generated>");
                }
                if key == "a=candidate" {
                    let mut fields: Vec<_> = value.split_whitespace().collect();
                    assert!(fields[5].parse::<u16>().unwrap() > 0);
                    fields[5] = "<ephemeral port>";
                    return format!("{key}:{}", fields.join(" "));
                }
                if key == "a=ssrc" {
                    let (id, attributes) = value.split_once(' ').unwrap();
                    assert!(id.parse::<u32>().is_ok());
                    return format!("{key}:<generated> {attributes}");
                }
            }
            if let Some(origin) = line.strip_prefix("o=") {
                let mut fields: Vec<_> = origin.split_whitespace().collect();
                assert!(fields[1].parse::<u64>().is_ok());
                fields[1] = "<session ID>";
                return format!("o={}", fields.join(" "));
            }
            line.to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
