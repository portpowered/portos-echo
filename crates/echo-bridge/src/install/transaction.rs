use std::path::{Path, PathBuf};

use echo_boot::{Result, layout};
use serde::{Deserialize, Serialize};

use super::{
    Install,
    files::{self, Files, ROOT},
    payload::{self, ASSETS, Release},
    policy::{self, Tool},
    wake::Wake,
};

mod effects;

use effects::{install_hook, install_policy, restore_hook, restore_mounts, restore_policy, set_fork};

const HOOK: &str = "\
service portos_echo_fw /data/local/echo/bin/boot boot firewall
    disabled
    seclabel u:r:su:s0
on boot
    start portos_echo_fw
service portos_echo /data/local/echo/bin/boot boot run
    disabled
    seclabel u:r:su:s0
on property:sys.boot_completed=1
    start portos_echo
";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Service {
    Running,
    Stopped,
    Absent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Step {
    Layout,
    WakeAssets,
    PrivateHome,
    Policy,
    Slots,
    ForkServices,
    InitHook,
    Mounts,
}

const STEPS: [Step; 8] = [
    Step::Layout,
    Step::WakeAssets,
    Step::Policy,
    Step::Slots,
    Step::ForkServices,
    Step::PrivateHome,
    Step::InitHook,
    Step::Mounts,
];

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Applying,
    Active,
    Restoring,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Install,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Services {
    hassmic: Service,
    hassmic_fw: Service,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fork {
    hook_present: bool,
    services: Services,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    phase: Phase,
    operation: Operation,
    in_flight: Option<Step>,
    completed: Vec<Step>,
    current_release: Option<String>,
    previous_release: Option<String>,
    fork: Fork,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Volume {
    System,
    Vendor,
}

pub(super) enum RebootFailure {
    Rejected(&'static str),
    Unknown,
}

pub(super) trait System: Tool {
    fn readonly(&mut self, volume: Volume) -> Result<bool>;
    fn remount(&mut self, volume: Volume, readonly: bool) -> Result<()>;
    fn service(&mut self, name: &'static str) -> Result<Service>;
    fn set_service(&mut self, name: &'static str, state: Service) -> Result<()>;
    fn label(&mut self, new: &Path, original: Option<&Path>) -> Result<()>;
    fn reboot(&mut self) -> std::result::Result<(), RebootFailure>;
    fn step(&mut self, step: Step, restoring: bool) -> Result<()>;
}

struct Paths {
    root: PathBuf,
    bin: PathBuf,
    work: PathBuf,
    hook: PathBuf,
    legacy_hook: PathBuf,
    saved_hook: PathBuf,
    policy: PathBuf,
    saved_policy: PathBuf,
    next_policy: PathBuf,
}

impl Paths {
    fn new(files: &Files<'_>) -> Self {
        let root = files.path(layout::ROOT);
        let bin = root.join(layout::BIN);
        Self {
            root,
            work: bin.join(".install"),
            bin,
            hook: files.path("/system/etc/init/portos-echo.rc"),
            legacy_hook: files.path("/system/etc/init/hassmic.rc"),
            saved_hook: files.path("/system/etc/init/.portos-echo/hassmic.saved"),
            policy: files.path(policy::PATH),
            saved_policy: files.path("/vendor/etc/selinux/.portos-echo/sepolicy.saved"),
            next_policy: files.path("/vendor/etc/selinux/.portos-echo.sepolicy.next"),
        }
    }

    fn backup_directories(&self) -> Result<[&Path; 2]> {
        let system = self.saved_hook.parent().ok_or("missing system backup directory")?;
        let vendor = self.saved_policy.parent().ok_or("missing vendor backup directory")?;
        Ok([system, vendor])
    }
}

pub(super) fn run(files: &Files<'_>, ops: &mut impl System, input: &Install) -> Result<&'static str> {
    let lock = files.protected(&files.path("/data/local"))?;
    lock.try_lock().map_err(|_| "installer already running")?;
    let paths = Paths::new(files);
    for path in [&paths.root, &paths.bin] {
        use std::os::unix::fs::MetadataExt;
        let metadata = files
            .protected(path)?
            .metadata()
            .map_err(|_| "inspect install layout")?;
        if files::mode(&metadata) != 0o755 || (files.root_uid == 0 && metadata.gid() != 0) {
            return Err("invalid staged install directory mode");
        }
    }
    if files::exists(&paths.work)? {
        return verify_active(files, &paths);
    }
    let stage = input.stage.as_ref().ok_or("fresh install requires --stage")?;
    let tool = files.path(input.magiskpolicy.to_str().ok_or("invalid policy tool path")?);
    ops.verify_policy_tool(files, &tool)?;
    let release = Release::verify(
        files,
        &files.path(stage.to_str().ok_or("invalid stage path")?),
        &paths.bin.join(layout::NEXT),
    )?;
    let plan = Plan::inspect(files, ops, &paths, release)?;
    let mut receipt = Receipt {
        version: 1,
        phase: Phase::Applying,
        operation: Operation::Install,
        in_flight: None,
        completed: Vec::new(),
        current_release: Some(plan.release.version.clone()),
        previous_release: None,
        fork: plan.fork,
    };
    let outcome = apply(files, ops, &paths, &plan, &mut receipt, &tool);
    if let Err(failed) = outcome {
        if undo(files, ops, &paths, &plan, &mut receipt).is_err() {
            return Err("install restoration incomplete; retain installer recovery record");
        }
        return Err(failed);
    }
    if input.reboot {
        match ops.reboot() {
            Err(RebootFailure::Rejected(failed)) => {
                undo(files, ops, &paths, &plan, &mut receipt)
                    .map_err(|_| "install restoration incomplete; retain installer recovery record")?;
                return Err(failed);
            }
            Err(RebootFailure::Unknown) => return Err("reboot outcome unknown; inspect active installer receipt"),
            Ok(()) => (),
        }
    }
    Ok("installed")
}

struct Plan {
    fork: Fork,
    release: Release,
    wake: Wake,
    system_readonly: bool,
    vendor_readonly: bool,
    private_missing: bool,
    home_missing: bool,
}

impl Plan {
    fn inspect(files: &Files<'_>, ops: &mut impl System, paths: &Paths, release: Release) -> Result<Self> {
        let [system_backup, vendor_backup] = paths.backup_directories()?;
        for path in [
            paths.hook.clone(),
            paths.hook.with_extension("new"),
            paths.next_policy.clone(),
            system_backup.to_path_buf(),
            vendor_backup.to_path_buf(),
            paths.bin.join("boot"),
            paths.bin.join(layout::CURRENT),
            paths.bin.join(layout::PREVIOUS),
            paths.bin.join(layout::BOOT_STATE),
        ]
        .into_iter()
        .chain(ASSETS.iter().map(|(_, name, _)| paths.bin.join(name)))
        {
            if files::exists(&path)? {
                return Err("conflicting install ownership");
            }
        }
        files.protected(paths.hook.parent().ok_or("missing init hook directory")?)?;
        files.trusted_file(&paths.policy)?;
        let hook_present = files::exists(&paths.legacy_hook)?;
        if hook_present {
            let hook = files::read_text(files.trusted_file(&paths.legacy_hook)?, 16 * 1024)?;
            if !["hassmic", "hassmic_fw"].iter().all(|name| {
                hook.lines()
                    .any(|line| line.starts_with(&format!("service {name} /system/bin/sh /system/hassmic/boot.sh ")))
            }) {
                return Err("unrecognized legacy init hook");
            }
        }
        let services = Services {
            hassmic: ops.service("hassmic")?,
            hassmic_fw: ops.service("hassmic_fw")?,
        };
        if !hook_present
            && !matches!(
                services,
                Services {
                    hassmic: Service::Absent,
                    hassmic_fw: Service::Absent
                }
            )
        {
            return Err("legacy services have no owned init hook");
        }
        let private = paths.root.join(layout::PRIVATE);
        let home = paths.root.join(layout::HOME);
        let private_missing = !files::exists(&private)?;
        let home_missing = !files::exists(&home)?;
        if !private_missing {
            payload::validate_retained_private(files, &private)?;
        }
        if !home_missing {
            payload::validate_retained_home(files, &home)?;
        }
        Ok(Self {
            fork: Fork { hook_present, services },
            release,
            wake: Wake::plan(files)?,
            system_readonly: ops.readonly(Volume::System)?,
            vendor_readonly: ops.readonly(Volume::Vendor)?,
            private_missing,
            home_missing,
        })
    }
}

fn apply(
    files: &Files<'_>,
    ops: &mut impl System,
    paths: &Paths,
    plan: &Plan,
    receipt: &mut Receipt,
    tool: &Path,
) -> Result<()> {
    files.directory(&paths.work, ROOT, 0o700)?;
    for step in STEPS {
        receipt.in_flight = Some(step);
        save(files, paths, receipt)?;
        apply_step(files, ops, paths, plan, tool, step)?;
        ops.step(step, false)?;
        receipt.completed.push(step);
        receipt.in_flight = None;
        save(files, paths, receipt)?;
    }
    receipt.phase = Phase::Active;
    save(files, paths, receipt)
}

fn apply_step(files: &Files<'_>, ops: &mut impl System, p: &Paths, plan: &Plan, tool: &Path, step: Step) -> Result<()> {
    match step {
        Step::Layout => files.directory(&p.bin.join(layout::BOOT_STATE), ROOT, 0o700),
        Step::WakeAssets => plan.wake.apply(files, &p.work.join("wake-model")),
        Step::PrivateHome => {
            payload::prepare_private(files, &plan.release, &p.work, &p.root.join(layout::PRIVATE))?;
            payload::prepare_home(files, &p.work, &p.root.join(layout::HOME))?;
            for (name, missing) in [
                (layout::PRIVATE, plan.private_missing),
                (layout::HOME, plan.home_missing),
            ] {
                if missing {
                    files::rename(&p.work.join(name), &p.root.join(name))?;
                }
            }
            Ok(())
        }
        Step::Policy => install_policy(files, ops, p, tool),
        Step::Slots => promote(files, p, &plan.release),
        Step::ForkServices => set_fork(ops, plan.fork.services, false),
        Step::InitHook => install_hook(files, ops, p, plan.fork.hook_present),
        Step::Mounts => restore_mounts(ops, plan),
    }
}

fn promote(files: &Files<'_>, p: &Paths, release: &Release) -> Result<()> {
    let next = p.bin.join(layout::NEXT);
    release.prepare_slots(files, &p.work, &next)?;
    let digest = files::digest(files.trusted_file(&next)?)?;
    files.write(&p.work.join("current.sha256"), digest.as_bytes(), ROOT, 0o600)?;
    for name in ASSETS.iter().map(|(_, name, _)| *name).chain(["boot"]) {
        files::rename(&p.work.join(name), &p.bin.join(name))?;
    }
    files::rename(&next, &p.bin.join(layout::CURRENT))
}

fn undo(files: &Files<'_>, ops: &mut impl System, p: &Paths, plan: &Plan, receipt: &mut Receipt) -> Result<()> {
    receipt.phase = Phase::Restoring;
    if !files::exists(&p.work)? {
        return Ok(());
    }
    let _journal = save(files, p, receipt);
    let restored = undo_steps(ops, p, plan, receipt);
    let mounts = restore_mounts(ops, plan);
    restored.and(mounts)?;
    files::remove(&p.work)
}

fn undo_steps(ops: &mut impl System, p: &Paths, plan: &Plan, receipt: &Receipt) -> Result<()> {
    let steps = receipt
        .completed
        .iter()
        .copied()
        .chain(receipt.in_flight)
        .collect::<Vec<_>>();
    for step in steps.into_iter().rev() {
        match step {
            Step::Mounts => (),
            Step::InitHook => restore_hook(ops, p)?,
            Step::ForkServices => set_fork(ops, plan.fork.services, true)?,
            Step::Slots => restore_slots(p)?,
            Step::Policy => restore_policy(ops, p)?,
            Step::PrivateHome => {
                if plan.home_missing {
                    files::remove(&p.root.join(layout::HOME))?;
                }
                if plan.private_missing {
                    files::remove(&p.root.join(layout::PRIVATE))?;
                }
            }
            Step::WakeAssets => plan.wake.undo()?,
            Step::Layout => files::remove(&p.bin.join(layout::BOOT_STATE))?,
        }
        ops.step(step, true)?;
    }
    Ok(())
}

fn restore_slots(p: &Paths) -> Result<()> {
    let current = p.bin.join(layout::CURRENT);
    if files::exists(&current)? {
        files::rename(&current, &p.bin.join(layout::NEXT))?;
    }
    for name in ASSETS.iter().map(|(_, name, _)| *name).chain(["boot"]) {
        files::remove(&p.bin.join(name))?;
    }
    Ok(())
}

fn save(files: &Files<'_>, p: &Paths, receipt: &Receipt) -> Result<()> {
    let encoded = serde_json::to_vec(receipt).map_err(|_| "encode install receipt")?;
    let temporary = p.work.join("receipt.new");
    files::remove(&temporary)?;
    files.write(&temporary, &encoded, ROOT, 0o600)?;
    files::rename(&temporary, &p.work.join("receipt.json"))
}

fn verify_active(files: &Files<'_>, p: &Paths) -> Result<&'static str> {
    if files::mode(
        &files
            .protected(&p.work)?
            .metadata()
            .map_err(|_| "inspect install receipt directory")?,
    ) != 0o700
    {
        return Err("unsafe installer receipt directory");
    }
    let Ok(file) = files.trusted_file(&p.work.join("receipt.json")) else {
        return Err("install recovery pending; retain installer recovery record");
    };
    let raw = files::read_text(file, 4096)?;
    let receipt: Receipt = serde_json::from_str(&raw).map_err(|_| "invalid install receipt")?;
    if receipt.version != 1
        || receipt.phase != Phase::Active
        || receipt.in_flight.is_some()
        || receipt.completed != STEPS
        || receipt.previous_release.is_some()
        || !receipt.current_release.as_deref().is_some_and(payload::valid_version)
    {
        return Err("install recovery pending; retain installer recovery record");
    }
    let hook = files::read_text(files.trusted_file(&p.hook)?, 4096)?;
    let expected = files::read_text(files.trusted_file(&p.work.join("current.sha256"))?, 64)?;
    let current = files::digest(files.trusted_file(&p.bin.join(layout::CURRENT))?)?;
    if hook != HOOK
        || current != expected
        || files::exists(&p.legacy_hook)?
        || !files::exists(&p.saved_policy)?
        || files::exists(&p.saved_hook)? != receipt.fork.hook_present
    {
        return Err("conflicting active install ownership");
    }
    Ok("unchanged")
}
