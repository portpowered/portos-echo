use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::Mutex,
};

use echo_boot::Result;

use super::{
    Install,
    files::{self, Files, Owner},
    parse,
    policy::Tool,
    transaction::{self, RebootFailure, Service, Step, System, Volume},
};

const HOOK_GOLDEN: &str = concat!(
    "service portos_echo_fw /data/local/echo/bin/boot boot firewall\n",
    "    disabled\n    seclabel u:r:su:s0\non boot\n    start portos_echo_fw\n",
    "service portos_echo /data/local/echo/bin/boot boot run\n",
    "    disabled\n    seclabel u:r:su:s0\non property:sys.boot_completed=1\n    start portos_echo\n",
);
const RULE_GOLDEN: &[&str] = &[
    "allow init su process { transition siginh rlimitinh noatsecure }",
    "allow su shell_exec file { read getattr map execute entrypoint open }",
    "allow su system_file file { read getattr map execute entrypoint open }",
    "allow su su process setcurrent",
    "allow domain su binder { call transfer }",
    "allow domain su fd use",
    "allow domain su process sigchld",
    "allow domain su fifo_file { read write getattr ioctl append lock map open }",
    "allow domain su file { read write getattr ioctl append lock map open }",
    "allow domain su unix_stream_socket { connectto read write getattr ioctl shutdown }",
    "allow netd su tcp_socket { read write getattr getopt setopt ioctl }",
    "allow netd su udp_socket { read write getattr getopt setopt ioctl }",
    "allow netd su rawip_socket { read write getattr getopt setopt ioctl }",
];
const LEGACY_HOOK: &str = "service hassmic /system/bin/sh /system/hassmic/boot.sh satellite\nservice hassmic_fw /system/bin/sh /system/hassmic/boot.sh firewall\n";
const CONFIG: &str = include_str!("../../../../examples/bridge.echo.toml");
const STOCK: &str = "system/local/models/keyword/en-US/COMPUTER";
const START: &str = "data/local/echo";

thread_local! {
    static OWNERS: Mutex<BTreeMap<u64, (u32, u32)>> = const { Mutex::new(BTreeMap::new()) };
}

fn record_owner(file: &File, Owner(uid, gid): Owner) -> Result<()> {
    OWNERS.with(|owners| {
        owners
            .lock()
            .unwrap()
            .insert(file.metadata().unwrap().ino(), (uid, gid))
    });
    Ok(())
}

struct Fixture {
    root: tempfile::TempDir,
    host: Fake,
}

impl Fixture {
    fn new(has_fork: bool) -> Self {
        OWNERS.with(|owners| owners.lock().unwrap().clear());
        let root = tempfile::tempdir().unwrap();
        let host = Fake::new(has_fork);
        let fixture = Self { root, host };
        fixture.write("vendor/etc/selinux/precompiled_sepolicy", b"original policy");
        fixture.write(&format!("{STOCK}/pryon.manifest"), b"stock model manifest");
        fixture.write(&format!("{STOCK}/computer.model"), b"stock model binary");
        fs::create_dir_all(fixture.path("system/etc/init")).unwrap();
        if has_fork {
            fixture.write("system/etc/init/hassmic.rc", LEGACY_HOOK.as_bytes());
            fixture.write("data/local/echo-bridge/private/bridge.toml", CONFIG.as_bytes());
            fixture.write("data/local/echo-bridge/private/run/stop", b"stale");
            fixture.write("data/local/echo-bridge/private/preferences", b"legacy bytes");
            fixture.write("data/local/echo-bridge/home/prompts/voice.md", b"legacy prompt");
            for name in ["private", "home"] {
                fs::set_permissions(
                    fixture.path(format!("data/local/echo-bridge/{name}")),
                    fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
        }
        fixture.stage();
        fixture
    }

    fn stage(&self) {
        self.write("data/local/stage/magiskpolicy", b"fake tool");
        let mut sums = BTreeMap::new();
        for (name, bytes) in [
            ("bin/echo-bridge", b"verified bridge".as_slice()),
            ("bin/codex-app-server", b"codex"),
            ("bin/cacert.pem", b"pem"),
            ("apk/echo-screen.apk", b"apk"),
            ("examples/bridge.echo.toml", CONFIG.as_bytes()),
        ] {
            let path = if name == "bin/echo-bridge" {
                format!("{START}/bin/next")
            } else {
                format!("data/local/stage/{name}")
            };
            self.write(&path, bytes);
            if name == "bin/echo-bridge" {
                fs::set_permissions(self.path(&path), fs::Permissions::from_mode(0o755)).unwrap();
            }
            sums.insert(name, files::digest(File::open(self.path(&path)).unwrap()).unwrap());
        }
        self.write(
            "data/local/stage/MANIFEST",
            &serde_json::to_vec(&serde_json::json!({"format":1,"version":"0.2.0","files":sums})).unwrap(),
        );
    }

    fn call(&mut self, reboot: bool) -> Result<&'static str> {
        self.call_staged(reboot, false)
    }

    fn call_staged(&mut self, reboot: bool, host_stage: bool) -> Result<&'static str> {
        let input = Install {
            host_stage,
            magiskpolicy: "/data/local/stage/magiskpolicy".into(),
            stage: Some("/data/local/stage".into()),
            reboot,
        };
        let root = self.root.path();
        let files = Files {
            root,
            root_uid: fs::metadata(root).unwrap().uid(),
            set_owner: record_owner,
        };
        transaction::run(&files, &mut self.host, &input)
    }

    fn path(&self, name: impl AsRef<Path>) -> PathBuf {
        self.root.path().join(name)
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}

struct Fake {
    fork: bool,
    mounts: (bool, bool),
    services: BTreeMap<&'static str, Service>,
    calls: Vec<String>,
    rules: Vec<String>,
    undo: Vec<Step>,
    fail: Option<Step>,
    reboot_failure: Option<RebootFailure>,
    fail_stop: bool,
}

impl Fake {
    fn new(fork: bool) -> Self {
        let initial = if fork {
            (Service::Running, Service::Stopped)
        } else {
            (Service::Absent, Service::Absent)
        };
        Self {
            fork,
            mounts: (true, true),
            services: BTreeMap::from([("hassmic", initial.0), ("hassmic_fw", initial.1)]),
            calls: Vec::new(),
            rules: Vec::new(),
            undo: Vec::new(),
            fail: None,
            reboot_failure: None,
            fail_stop: false,
        }
    }
}

impl Tool for Fake {
    fn verify_policy_tool(&self, files: &Files<'_>, tool: &Path) -> Result<()> {
        assert_eq!(files::read_text(files.trusted_file(tool)?, 100)?, "fake tool");
        Ok(())
    }

    fn policy(&mut self, _tool: &Path, args: &[&Path], print: bool) -> Result<String> {
        assert_eq!(args[0], Path::new("--load"));
        self.calls.push("policy".into());
        if print {
            assert_eq!(args[2], Path::new("--print-rules"));
            let base =
                "type su { domain }\nallow init su process { transition }\nallow netd su tcp_socket { getopt }\n";
            return Ok(if self.fork || args[1].ends_with("policy.next") {
                format!("{base}type hassmic {{ domain }}\n")
            } else {
                base.to_owned()
            });
        }
        assert_eq!(args[2], Path::new("--apply"));
        assert_eq!(args[4], Path::new("--save"));
        self.rules.push(fs::read_to_string(args[3]).unwrap());
        fs::write(args[5], "compiled policy").unwrap();
        Ok(String::new())
    }
}

impl System for Fake {
    fn readonly(&mut self, volume: Volume) -> Result<bool> {
        Ok(match volume {
            Volume::System => self.mounts.0,
            Volume::Vendor => self.mounts.1,
        })
    }

    fn remount(&mut self, volume: Volume, readonly: bool) -> Result<()> {
        let (name, current) = match volume {
            Volume::System => ("system", &mut self.mounts.0),
            Volume::Vendor => ("vendor", &mut self.mounts.1),
        };
        *current = readonly;
        self.calls.push(format!("mount {name} {readonly}"));
        Ok(())
    }

    fn service(&mut self, name: &'static str) -> Result<Service> {
        Ok(self.services[name])
    }

    fn set_service(&mut self, name: &'static str, state: Service) -> Result<()> {
        self.calls.push(format!("service {name} {state:?}"));
        self.services.insert(name, state);
        if self.fail_stop && state == Service::Stopped {
            return Err("seeded stop failure");
        }
        Ok(())
    }

    fn label(&mut self, _new: &Path, _: Option<&Path>) -> Result<()> {
        Ok(())
    }

    fn reboot(&mut self) -> std::result::Result<(), RebootFailure> {
        if let Some(failure) = self.reboot_failure {
            return Err(failure);
        }
        self.calls.push("reboot".into());
        Ok(())
    }

    fn step(&mut self, step: Step, restoring: bool) -> Result<()> {
        if restoring {
            self.undo.push(step);
        }
        if self.fail == Some(step) && !restoring {
            return Err("seeded install failure");
        }
        Ok(())
    }
}

#[test]
fn t3_a2_device_install_fork_takeover_and_repeat() {
    let mut fixture = Fixture::new(true);
    assert_eq!(fixture.call(false), Ok("installed"));
    assert_eq!(
        fs::read_to_string(fixture.path("system/etc/init/portos-echo.rc")).unwrap(),
        HOOK_GOLDEN
    );
    assert_eq!(fixture.host.rules[0].lines().collect::<Vec<_>>(), RULE_GOLDEN);
    assert_eq!(fixture.host.rules.len(), 1);
    assert_eq!(
        fixture.host.calls,
        [
            "policy",
            "policy",
            "mount vendor false",
            "service hassmic Stopped",
            "mount system false",
            "mount system true",
            "mount vendor true"
        ]
    );
    assert_eq!(
        fixture.host.services.values().copied().collect::<Vec<_>>(),
        [Service::Stopped; 2]
    );
    assert!(!fixture.path("system/etc/init/hassmic.rc").exists());
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.path(format!("{START}/bin/.install/receipt.json"))).unwrap()).unwrap();
    assert_eq!(receipt["phase"], "active");
    assert_eq!(receipt["current_release"], "0.2.0");
    assert_eq!(
        receipt["fork"]["services"],
        serde_json::json!({"hassmic":"running","hassmic_fw":"stopped"})
    );
    assert_eq!(
        fs::read(fixture.path(format!("{START}/private/preferences"))).unwrap(),
        b"legacy bytes"
    );
    assert_eq!(
        fs::read(fixture.path(format!("{START}/home/prompts/voice.md"))).unwrap(),
        b"legacy prompt"
    );
    assert!(!fixture.path(format!("{START}/private/run/stop")).exists());
    let config = fs::read_to_string(fixture.path(format!("{START}/private/bridge.toml"))).unwrap();
    assert!(config.contains("/data/local/echo/bin/codex-app-server"));
    assert_eq!(
        fs::read_to_string(fixture.path("data/local/echo-bridge/private/bridge.toml")).unwrap(),
        CONFIG
    );
    check_metadata(&fixture);
    let before = snapshot(fixture.root.path());
    fixture.host.calls.clear();
    assert_eq!(fixture.call(true), Ok("unchanged"));
    assert_eq!(snapshot(fixture.root.path()), before);
    assert_eq!(fixture.host.calls, Vec::<String>::new());
}

fn check_metadata(fixture: &Fixture) {
    for (name, expected, owner) in [
        ("bin/boot", 0o755, (0, 0)),
        ("bin/current", 0o755, (0, 0)),
        ("bin/.boot", 0o700, (0, 0)),
        ("bin/.install", 0o700, (0, 0)),
        ("bin/.install/receipt.json", 0o600, (0, 0)),
        ("bin/codex-app-server", 0o755, (0, 0)),
        ("bin/cacert.pem", 0o644, (0, 0)),
        ("bin/echo-screen.apk", 0o644, (0, 0)),
        ("private", 0o700, (9999, 9999)),
        ("private/run", 0o700, (9999, 9999)),
        ("private/bridge.toml", 0o600, (9999, 9999)),
        ("home", 0o700, (9999, 9999)),
    ] {
        let metadata = fs::metadata(fixture.path(format!("{START}/{name}"))).unwrap();
        assert_eq!(files::mode(&metadata), expected, "{name}");
        if name != "bin/current" {
            OWNERS.with(|owners| assert_eq!(owners.lock().unwrap()[&metadata.ino()], owner, "{name}"));
        }
    }
}

#[test]
fn t3_a2_device_install_bare_root_prepares_confined_wake() {
    let mut fixture = Fixture::new(false);
    assert_eq!(fixture.call(true), Ok("installed"));
    assert_eq!(fixture.host.rules[0].lines().collect::<Vec<_>>(), RULE_GOLDEN);
    assert!(fixture.host.rules[1].contains("deny hassmic * property_service *\n"));
    assert!(fixture.host.rules[1].contains("allow netd hassmic tcp_socket { getopt }\n"));
    assert_eq!(fixture.host.calls.last().map(String::as_str), Some("reboot"));
    assert_eq!(
        fs::read(fixture.path("data/local/hassmic/models/computer-en-US/computer.model")).unwrap(),
        b"stock model binary"
    );
    assert_eq!(
        fs::read(fixture.path("data/local/hassmic/state/wake_word")).unwrap(),
        b"computer-en-US\n"
    );
    assert_eq!(
        fs::read(fixture.path(format!("{START}/bin/current"))).unwrap(),
        b"verified bridge"
    );
    check_metadata(&fixture);
}

#[test]
fn t3_a2_device_install_stages_verified_host_candidate_under_lock_and_restores_next() {
    let mut fixture = Fixture::new(false);
    let next = fixture.path(format!("{START}/bin/next"));
    let candidate = fixture.path("data/local/stage/bin/echo-bridge");
    fs::rename(&next, &candidate).unwrap();
    let before = snapshot(fixture.root.path());
    fixture.host.fail = Some(Step::InitHook);
    assert_eq!(fixture.call_staged(false, true), Err("seeded install failure"));
    assert_eq!(snapshot(fixture.root.path()), before);
    fixture.host.fail = None;
    assert_eq!(fixture.call_staged(false, true), Ok("installed"));
    assert!(!next.exists());
    assert_eq!(
        fs::read(fixture.path(format!("{START}/bin/current"))).unwrap(),
        b"verified bridge"
    );
    assert_eq!(fs::read(&candidate).unwrap(), b"verified bridge");
}

#[test]
fn t3_a2_device_install_rolls_back_each_failed_step() {
    let steps = [
        Step::Layout,
        Step::WakeAssets,
        Step::Policy,
        Step::Slots,
        Step::ForkServices,
        Step::PrivateHome,
        Step::InitHook,
        Step::Mounts,
    ];
    for (index, step) in steps.iter().enumerate() {
        let mut fixture = Fixture::new(true);
        let before = snapshot(fixture.root.path());
        fixture.host.fail = Some(*step);
        assert_eq!(fixture.call(true), Err("seeded install failure"), "{step:?}");
        assert_restored(&fixture, &before);
        assert_eq!(
            fixture.host.undo,
            steps[..=index].iter().rev().copied().collect::<Vec<_>>()
        );
    }
    let mut fixture = Fixture::new(true);
    let before = snapshot(fixture.root.path());
    fixture.host.reboot_failure = Some(RebootFailure::Rejected("seeded reboot rejection"));
    assert_eq!(fixture.call(true), Err("seeded reboot rejection"));
    assert_restored(&fixture, &before);
    fixture.host.fail_stop = true;
    assert_eq!(fixture.call(true), Err("seeded stop failure"));
    assert_restored(&fixture, &before);
}

#[test]
fn t3_a2_device_install_rejects_symlink_and_parallel_writer() {
    let mut fixture = Fixture::new(true);
    let lock = File::open(fixture.path("data/local")).unwrap();
    lock.try_lock().unwrap();
    assert_eq!(fixture.call(false), Err("installer already running"));
    drop(lock);
    let entry = fixture.path("data/local/stage/bin/codex-app-server");
    fs::remove_file(&entry).unwrap();
    symlink(fixture.path(format!("{START}/bin/next")), entry).unwrap();
    assert_eq!(fixture.call(false), Err("open install file"));
    assert!(!fixture.path(format!("{START}/bin/.install")).exists());
    assert_eq!(fixture.host.calls, Vec::<String>::new());
    let parsed = parse(
        ["--magiskpolicy", "/local/../untrusted"]
            .into_iter()
            .map(OsString::from)
            .collect(),
    );
    assert!(parsed.is_none());
}

#[test]
fn t3_a2_device_install_keeps_receipt_when_reboot_is_ambiguous() {
    let mut fixture = Fixture::new(false);
    fixture.host.reboot_failure = Some(RebootFailure::Unknown);
    assert_eq!(
        fixture.call(true),
        Err("reboot outcome unknown; inspect active installer receipt")
    );
    let before = snapshot(fixture.root.path());
    fixture.host.calls.clear();
    assert_eq!(fixture.call(true), Ok("unchanged"));
    assert_eq!(snapshot(fixture.root.path()), before);
    assert_eq!(fixture.host.calls, Vec::<String>::new());
}

type Snapshot = BTreeMap<PathBuf, (u32, u32, u32, Option<Vec<u8>>)>;

fn snapshot(root: &Path) -> Snapshot {
    fn walk(directory: &Path, result: &mut Snapshot) {
        for path in fs::read_dir(directory).unwrap().map(|entry| entry.unwrap().path()) {
            let meta = fs::symlink_metadata(&path).unwrap();
            let content = if meta.is_dir() {
                walk(&path, result);
                None
            } else {
                Some(fs::read(&path).unwrap())
            };
            result.insert(path, (meta.uid(), meta.gid(), files::mode(&meta), content));
        }
    }
    let mut paths = BTreeMap::new();
    walk(root, &mut paths);
    paths
}

fn assert_restored(fixture: &Fixture, before: &Snapshot) {
    assert_eq!(&snapshot(fixture.root.path()), before);
    assert_eq!(fixture.host.mounts, (true, true));
    assert_eq!(fixture.host.services["hassmic"], Service::Running);
    assert_eq!(fixture.host.services["hassmic_fw"], Service::Stopped);
    assert!(!fixture.host.calls.iter().any(|event| event == "reboot"));
}
