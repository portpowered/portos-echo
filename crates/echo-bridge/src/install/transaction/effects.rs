use std::{fs, path::Path};

use echo_boot::Result;

use super::{Files, HOOK, Paths, Plan, ROOT, Service, Services, System, Volume, files, policy};

pub(super) fn install_policy(files: &Files<'_>, ops: &mut impl System, p: &Paths, tool: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    policy::prepare(files, ops, tool, &p.work)?;
    if files::digest(files.trusted_file(&p.policy)?)?
        != files::digest(files.trusted_file(&p.work.join("policy.original"))?)?
    {
        return Err("policy changed during installer preparation");
    }
    let original = files
        .trusted_file(&p.policy)?
        .metadata()
        .map_err(|_| "inspect policy preimage")?;
    mount(ops, Volume::Vendor, false)?;
    files.copy(
        files.trusted_file(&p.work.join("policy.next"))?,
        &p.next_policy,
        files::Owner(0, original.gid()),
        files::mode(&original),
    )?;
    ops.label(&p.next_policy, Some(&p.policy))?;
    let [_, backup] = p.backup_directories()?;
    files.directory(backup, ROOT, 0o700)?;
    fs::hard_link(&p.policy, &p.saved_policy).map_err(|_| "save policy preimage")?;
    files::open_dir(backup)?
        .sync_all()
        .map_err(|_| "sync policy preimage")?;
    files::rename(&p.next_policy, &p.policy)?;
    for name in [
        "model.rules",
        "domain.rules",
        "policy.original",
        "policy.model",
        "policy.next",
    ] {
        files::remove(&p.work.join(name))?;
    }
    Ok(())
}

pub(super) fn install_hook(files: &Files<'_>, ops: &mut impl System, p: &Paths, has_fork: bool) -> Result<()> {
    mount(ops, Volume::System, false)?;
    let staged = p.hook.with_extension("new");
    files.write(&staged, HOOK.as_bytes(), ROOT, 0o644)?;
    ops.label(&staged, None)?;
    if has_fork {
        let [backup, _] = p.backup_directories()?;
        files.directory(backup, ROOT, 0o700)?;
        fs::hard_link(&p.legacy_hook, &p.saved_hook).map_err(|_| "save legacy hook")?;
        files::open_dir(backup)?.sync_all().map_err(|_| "sync legacy hook")?;
        // Keep one boot hook visible even if the device loses power between renames.
        files::rename(&staged, &p.legacy_hook)?;
        files::rename(&p.legacy_hook, &p.hook)
    } else {
        files::rename(&staged, &p.hook)
    }
}

pub(super) fn restore_hook(ops: &mut impl System, p: &Paths) -> Result<()> {
    let staged = p.hook.with_extension("new");
    let [backup, _] = p.backup_directories()?;
    if files::exists(&p.hook)? || files::exists(backup)? || files::exists(&staged)? {
        mount(ops, Volume::System, false)?;
        if files::exists(&p.saved_hook)? {
            if files::exists(&p.hook)? {
                files::rename(&p.hook, &p.legacy_hook)?;
            }
            files::rename(&p.saved_hook, &p.legacy_hook)?;
        } else {
            files::remove(&p.hook)?;
        }
        files::remove(&staged)?;
        files::remove(backup)?;
    }
    Ok(())
}

pub(super) fn restore_policy(ops: &mut impl System, p: &Paths) -> Result<()> {
    let [_, backup] = p.backup_directories()?;
    if files::exists(backup)? || files::exists(&p.next_policy)? {
        mount(ops, Volume::Vendor, false)?;
        if files::exists(&p.saved_policy)? {
            files::rename(&p.saved_policy, &p.policy)?;
            // Renaming two links to the same inode is a no-op.
            files::remove(&p.saved_policy)?;
        }
        files::remove(&p.next_policy)?;
        files::remove(backup)?;
    }
    Ok(())
}

pub(super) fn restore_mounts(ops: &mut impl System, plan: &Plan) -> Result<()> {
    let system = mount(ops, Volume::System, plan.system_readonly);
    let vendor = mount(ops, Volume::Vendor, plan.vendor_readonly);
    system.and(vendor)
}

fn mount(ops: &mut impl System, volume: Volume, readonly: bool) -> Result<()> {
    if ops.readonly(volume)? != readonly {
        ops.remount(volume, readonly)?;
    }
    Ok(())
}

pub(super) fn set_fork(ops: &mut impl System, initial: Services, restoring: bool) -> Result<()> {
    for (name, state) in [("hassmic", initial.hassmic), ("hassmic_fw", initial.hassmic_fw)] {
        if state == Service::Absent {
            continue;
        }
        let target = if restoring { state } else { Service::Stopped };
        if ops.service(name)? != target {
            ops.set_service(name, target)?;
        }
    }
    Ok(())
}
