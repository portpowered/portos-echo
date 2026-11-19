//! T2: every adb call lands in a synthetic ledger. No adb, Echo, Ring or account is used.
#[path = "journey_tests.rs"]
mod journey;

use super::*;
use crate::lab_run::{
    Step,
    device::{Configured, config},
    run_options::{self, Options},
    run_report::Artifacts,
};
use serde_json::json;
use std::{cell::RefCell, rc::Rc};

const OWNER: &str = "9999:9999"; // lint-rules: allow-literal synthetic uid:gid ownership metadata, no connection
const CHOWN: &str = "chown 9999:9999 "; // lint-rules: allow-literal synthetic uid:gid ownership metadata, no connection
const PRIVATE: &str = "/data/local/echo-bridge/private";
const UNLINKED: &str = "device: unmet prerequisite: hassmic link to the bridge on the Echo; its log shows no \
\"client connected\" or \"satellite: running\" after the last \"client disconnected\"";

/// The Echo as adb shows it: one trace, one hassmic log and at most one bridge process.
#[derive(Default)]
pub(super) struct World {
    pub calls: Vec<Vec<String>>,
    pub trace: String,
    pub boot: String,
    pub log: String,
    pub links: bool,
    pub alive: bool,
    pub pid: u32,
    pub mode: String,
    pub owner: String,
    pub fail: String,
    pub forward: u16,
    pub pushed: Vec<(String, String, String)>,
}

impl World {
    fn shell(&mut self, args: &[String], command: &str) -> String {
        if args[1] == "ps" {
            let bridge = if self.alive {
                format!("{} echo-bridge\n", self.pid)
            } else {
                String::new()
            };
            return format!("PID NAME\n1 init\n77 sh\n{bridge}");
        }
        if command.contains("setsid") {
            self.alive = true;
            self.pid += 1;
            self.trace = self.boot.clone();
            if self.links {
                self.log.push_str("hassmic: client connected\n");
            }
        } else if command.contains("kill $(cat") {
            self.alive = false;
        } else if command.contains("rm -f") && command.contains("trace.jsonl") {
            self.trace.clear();
        } else if command.contains("wc -c") {
            return format!("{} log\n", self.log.len());
        } else if command.contains("stat -c") {
            return format!("{}:{}\n", self.owner, self.mode);
        }
        String::new()
    }

    pub fn ran(&self, text: &str) -> bool {
        self.calls.iter().any(|call| call.join(" ").contains(text))
    }
}

pub(super) struct Fake(pub Rc<RefCell<World>>);

impl Shell for Fake {
    fn adb(&self, args: &[String]) -> Result<Vec<u8>, String> {
        let mut world = self.0.borrow_mut();
        world.calls.push(args.to_vec());
        let command = args.last().cloned().unwrap_or_default();
        if !world.fail.is_empty() && command.contains(&world.fail) {
            return Err("synthetic staging failure".into());
        }
        if command.starts_with(IDENTITY) && command.contains("bridge.toml") && world.owner != OWNER {
            return Err("synthetic nobody access denied".into());
        }
        let output = match args[0].as_str() {
            "push" => {
                let contents = std::fs::read_to_string(&args[1]).map_err(|_| "push: unreadable")?;
                world.pushed.push((args[1].clone(), args[2].clone(), contents));
                world.owner = "0:0".into(); // lint-rules: allow-literal synthetic uid:gid ownership metadata, no connection
                String::new()
            }
            "forward" if args[1] == "tcp:0" => format!("{}\n", world.forward), // lint-rules: allow-literal synthetic adb argument comparison, no connection
            "exec-out" if command.contains("cat ") => world.trace.clone(),
            "exec-out" => {
                let from = command
                    .split("tail -c +")
                    .nth(1)
                    .and_then(|rest| rest.split(' ').next());
                let from: usize = from.and_then(|from| from.parse().ok()).ok_or("tail: no offset")?;
                world.log[from - 1..].to_owned()
            }
            "shell" if command.starts_with(CHOWN) => {
                world.owner = OWNER.into();
                String::new()
            }
            "shell" => world.shell(args, &command),
            _ => String::new(),
        };
        Ok(output.into_bytes())
    }
}

pub(super) fn world() -> Rc<RefCell<World>> {
    Rc::new(RefCell::new(World {
        boot: [
            json!({"event":"screen.listening","elapsed_ms":1,"address":"127.0.0.1:16900"}),
            json!({"event":"bridge.ready","elapsed_ms":2}),
        ]
        .map(|record| format!("{record}\n"))
        .concat(),
        links: true,
        mode: "600".into(),
        owner: OWNER.into(),
        ..World::default()
    }))
}

fn launcher_json() -> serde_json::Value {
    json!({"private_dir":PRIVATE,"config":format!("{PRIVATE}/bridge.toml"),
        "trace":format!("{PRIVATE}/trace.jsonl"),"hassmic_log":"/data/local/hassmic/boot.log",
        "binary":"/data/local/echo-bridge/bin/echo-bridge","run_as":"runas nobody 3990,inet"})
}

fn select(root: &std::path::Path, value: &serde_json::Value) -> Result<Option<Launcher>, String> {
    let path = root.join("launcher.json");
    std::fs::write(&path, value.to_string()).unwrap();
    Launcher::select(Some(&path.to_string_lossy().into_owned()), false, true)
}

pub(super) fn remote(world: &Rc<RefCell<World>>) -> Remote {
    let root = tempfile::tempdir().unwrap();
    let launcher = select(root.path(), &launcher_json()).unwrap().unwrap();
    Remote::new(launcher, Box::new(Fake(world.clone())))
}

fn arguments(root: &std::path::Path, scenario: &str) -> Vec<String> {
    let template = include_str!("../../../../../../examples/bridge.example.toml");
    for (name, contents) in [
        ("port", String::new()),
        ("adb", String::new()),
        ("bridge.toml", template.replace("echo.example.invalid", "127.0.0.1")),
        ("launcher.json", launcher_json().to_string()),
    ] {
        std::fs::write(root.join(name), contents).unwrap();
    }
    let file = |name: &str| root.join(name).to_string_lossy().into_owned();
    let scenario = format!("{}/../../tests/scenarios/{scenario}.yaml", env!("CARGO_MANIFEST_DIR"));
    let fixed = "--driver device --bench ring --live real --stt stt --ring synthetic --speech-model synthetic";
    let mut args: Vec<String> = fixed.split(' ').map(String::from).collect();
    for (flag, value) in [
        ("--speech-endpoint", "http://127.0.0.1:1/v1".to_owned()),
        ("--port", file("port")),
        ("--adb", file("adb")),
        ("--bridge-config", file("bridge.toml")),
        ("--device-bridge", file("launcher.json")),
        ("--out", file("out")),
    ] {
        args.extend([flag.to_owned(), value]);
    }
    args.push(scenario);
    args
}

pub(super) fn parsed(root: &std::path::Path, scenario: &str) -> Options {
    run_options::parse(&arguments(root, scenario)).unwrap()
}

#[test]
fn t2_on_device_mode_loads_m1_and_m4_with_no_host_bridge_or_wire_relay() {
    for (scenario, milestone, spoof) in [("m1-hassmic-bridge", "M1", None), ("m4-spoof-tool", "M4", Some(18080))] {
        let root = tempfile::tempdir().unwrap();
        let options = parsed(root.path(), scenario);
        assert!(options.bridge.is_none() && options.on_device.is_some() && options.live_real());
        let (loaded, tier, steps) = crate::lab_run::load(&options).unwrap();
        assert_eq!((loaded.as_str(), tier.as_str()), (milestone, "T5"));
        assert_eq!(
            steps.iter().any(|step| matches!(step, Step::SayText(_))),
            milestone == "M4"
        );
        let operator = std::fs::read_to_string(root.path().join("bridge.toml")).unwrap();
        let derived = config::load(&options).unwrap().unwrap();
        assert_eq!(derived, operator);
        let mut errors = Vec::new();
        let configured = Configured::new(&options, derived, &mut errors).ok().unwrap();
        assert!(configured.state.remote.is_some() && configured.wire_endpoint.is_none());
        assert_eq!(configured.window, std::time::Duration::from_secs(50));
        assert_eq!(configured.state.spoof_port, spoof);
    }
}

#[test]
fn t2_on_device_mode_refuses_wire_judged_journeys_and_incoherent_flags_by_name() {
    let root = tempfile::tempdir().unwrap();
    let options = parsed(root.path(), "session-two-turns");
    let derived = config::load(&options).unwrap().unwrap();
    let mut errors = Vec::new();
    assert_eq!(
        Configured::new(&options, derived, &mut errors).err().unwrap(),
        "device: unmet prerequisite: session-two-turns needs the host wire observer; a bridge on the Echo has no relay in its Wyoming path"
    );
    let flags = "device: --device-bridge replaces --bridge and requires --bridge-config with --live real";
    let mut fake_live = arguments(root.path(), "m1-hassmic-bridge");
    fake_live.retain(|arg| arg != "--live" && arg != "real");
    assert_eq!(run_options::parse(&fake_live).err().unwrap(), flags);
    let path = root.path().join("launcher.json").to_string_lossy().into_owned();
    assert_eq!(Launcher::select(Some(&path), true, true).unwrap_err(), flags);
    assert!(Launcher::select(None, true, false).unwrap().is_none());
}

#[test]
fn t2_launcher_accepts_only_inert_paths_inside_one_private_directory() {
    let root = tempfile::tempdir().unwrap();
    let edits: [(&str, serde_json::Value); 9] = [
        ("config", json!("/data/local/elsewhere/bridge.toml")),
        ("trace", json!("relative/trace.jsonl")),
        ("trace", json!(format!("{PRIVATE}/../trace.jsonl"))),
        ("private_dir", json!("/data/local/echo-bridge/private; reboot")),
        ("hassmic_log", json!("/data/local/hassmic/$(reboot)")),
        ("process", json!("a-name-longer-than-the-kernel-keeps")),
        ("run_as", json!(format!("runas {}; reboot", "root"))),
        ("start", json!(["shell", "start"])),
        ("unknown", json!(true)),
    ];
    for (key, value) in edits {
        let mut launcher = launcher_json();
        launcher[key] = value;
        assert_eq!(
            select(root.path(), &launcher).unwrap_err(),
            "device: invalid on-device launcher",
            "{key}"
        );
    }
    let mut nominated = launcher_json();
    nominated.as_object_mut().unwrap().remove("binary");
    nominated["start"] = json!(["shell", "start-bridge {config} {trace}"]);
    nominated["stop"] = json!(["shell", "stop-bridge {private_dir}"]);
    let launcher = select(root.path(), &nominated).unwrap().unwrap();
    assert_eq!(
        launcher.command(launcher.start.as_ref(), START),
        [
            "shell",
            &format!("{IDENTITY} sh -c 'start-bridge {PRIVATE}/bridge.toml {PRIVATE}/trace.jsonl'")
        ]
    );
    assert_eq!(
        launcher.command(launcher.stop.as_ref(), STOP),
        ["shell", &format!("{IDENTITY} sh -c 'stop-bridge {PRIVATE}'")]
    );
}

#[test]
fn t2_liveness_is_the_named_process_never_the_launching_shell() {
    let world = world();
    let remote = remote(&world);
    assert_eq!(remote.pid(), Ok(None));
    world.borrow_mut().alive = true;
    world.borrow_mut().pid = 41;
    assert_eq!(remote.pid(), Ok(Some(41)));
    let world = world.borrow();
    assert!(world.ran("ps -A -o PID,NAME") && !world.ran("pgrep"));
}

#[test]
fn t2_m4_budget_passes_in_budget_and_fails_with_the_measured_number() {
    // The row echo-bridge writes after a settled reply, field for field.
    let timing = |run: &str, duration: u64| {
        json!({"event":"timing","elapsed_ms":5,"provider_auth_mode":"chatgpt","run_id":run,
            "name":"first_audio_after_endpoint_ms","duration_ms":duration,"endpoint":"user_transcript"})
    };
    let measured = budget::check(&[timing("run-1", 1200), timing("run-0", 9000)], Some("run-1"), 1500.0).unwrap();
    assert_eq!(
        (&measured["measured"], &measured["samples"]),
        (&json!(1200.0), &json!(1))
    );
    assert_eq!(
        budget::check(&[timing("run-1", 1501)], Some("run-1"), 1500.0).unwrap_err(),
        "device: first_audio_after_endpoint_ms_p95 observed 1501 ms exceeds 1500 ms"
    );
    assert_eq!(
        budget::check(&[timing("run-0", 1200)], Some("run-1"), 1500.0).unwrap_err(),
        "device: unmet prerequisite: first_audio_after_endpoint_ms timing sample in the bridge trace from the Echo"
    );
    for (duration, verdict) in [(1500, true), (1501, false)] {
        let root = tempfile::tempdir().unwrap();
        let options = parsed(root.path(), "m4-spoof-tool");
        let mut errors = Vec::new();
        let derived = config::load(&options).unwrap().unwrap();
        let mut configured = Configured::new(&options, derived, &mut errors).ok().unwrap();
        let world = world();
        world.borrow_mut().trace = format!("{}\n", timing("run-1", duration));
        configured.state.remote = Some(remote(&world));
        configured.state.started = true;
        configured.state.run = Some("run-1".into());
        let mut artifacts = Artifacts::create(root.path().join("report")).unwrap();
        let result = configured.trace_budget(&mut artifacts);
        assert_eq!(result.is_ok(), verdict, "{result:?}");
        assert_eq!(artifacts.checks["device_m4_latency"], verdict);
    }
}

#[test]
fn t2_identity_defaults_normalize_and_root_is_refused_before_effects() {
    let root = tempfile::tempdir().unwrap();
    let world = world();
    for spelling in [None, Some(""), Some("runas nobody 3990,inet"), Some(IDENTITY)] {
        let mut value = launcher_json();
        value.as_object_mut().unwrap().remove("run_as");
        if let Some(spelling) = spelling {
            value["run_as"] = json!(spelling);
        }
        let launcher = select(root.path(), &value).unwrap().unwrap();
        assert!(launcher.command(None, START)[1].starts_with(&format!("{IDENTITY} sh -c '")));
    }
    for helper in ["runas", "/data/local/hassmic/runas"] {
        for user in ["root", "0"] {
            let mut value = launcher_json();
            value["run_as"] = json!(format!("{helper} {user} 3990,inet"));
            assert_eq!(select(root.path(), &value).unwrap_err(), ROOT_REFUSAL);
        }
    }
    assert_eq!(world.borrow().calls, Vec::<Vec<String>>::new());
    for args in [
        json!(["reboot"]),
        json!(["exec-out", "payload"]),
        json!(["shell", "one", "two"]),
    ] {
        let mut value = launcher_json();
        value.as_object_mut().unwrap().remove("binary");
        value["start"] = args;
        value["stop"] = json!(["shell", "stop"]);
        assert_eq!(select(root.path(), &value).unwrap_err(), INVALID);
    }
}

#[test]
fn t2_nobody_cannot_chmod_a_root_staged_file_until_ownership_transfer() {
    let root = tempfile::tempdir().unwrap();
    let local = root.path().join("config");
    std::fs::write(&local, "derived").unwrap();
    let world = world();
    let remote = remote(&world);
    remote
        .shell
        .adb(&[
            "push".into(),
            local.to_string_lossy().into_owned(),
            format!("{PRIVATE}/bridge.toml"),
        ])
        .unwrap();
    assert_eq!(
        remote.shell(&format!("chmod 600 {PRIVATE}/bridge.toml")).unwrap_err(),
        "synthetic nobody access denied"
    );
    remote.stage(&local).unwrap();
    assert_eq!(world.borrow().owner, OWNER);
    assert_eq!(world.borrow().mode, "600");
}
