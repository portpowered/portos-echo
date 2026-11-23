use std::{
    fmt::Write as _,
    fs::{self, File, Metadata},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

use echo_boot::{Result, Role, layout};
use echo_core_api::{Clock, SystemClock};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::super::files::Files;
use super::{CONFIG, child_succeeded, private};

const TRACE_LIMIT: u64 = 64 * 1024 * 1024;
const READY: &str = "wake=confirmed\nlink=connected\nrun=started";
const PENDING: &str = "wake=pending\nlink=pending\nrun=pending";

pub(super) fn status(files: &Files<'_>, version: &str) -> Result<String> {
    let (core, device) = roles(files)?;
    let (process, user) = match core {
        Some(true) => ("running", "nobody"),
        Some(false) => ("running", "unavailable"),
        None => ("stopped", "unavailable"),
    };
    let link = if device {
        "client connected"
    } else {
        "no Home Assistant link"
    };
    Ok(format!(
        "process={process}\nprocess_user={user}\nuptime_seconds=unavailable\nhassmic_link={link}\nversion_current={version}\nversion_previous=null"
    ))
}

pub(super) fn core_running(files: &Files<'_>) -> Result<bool> {
    match roles(files)?.0 {
        Some(false) => Err("unsafe running A2 core"),
        value => Ok(value.is_some()),
    }
}

fn roles(files: &Files<'_>) -> Result<(Option<bool>, bool)> {
    let image = files
        .trusted_file(&files.path("/data/local/echo/bin/current"))?
        .metadata()
        .map_err(|_| "inspect current image")?;
    let mut core = None;
    let mut device = false;
    for entry in fs::read_dir("/proc").map_err(|_| "read A2 processes")? {
        let Ok(entry) = entry else { continue };
        if !entry.file_name().as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let path = entry.path();
        if !fs::metadata(path.join("exe")).is_ok_and(|exe| identity(&exe) == identity(&image)) {
            continue;
        }
        match role(&path) {
            Some((Role::Core, allowed)) => {
                core = Some(core.is_none() && allowed);
            }
            Some((Role::Device, true)) => device = true,
            _ => (),
        }
    }
    let socket = fs::symlink_metadata(echo_device::WYOMING_SOCKET).is_ok_and(|file| file.file_type().is_socket());
    Ok((core, device && socket))
}

fn role(path: &Path) -> Option<(Role, bool)> {
    let argv = fs::read(path.join("cmdline")).ok()?;
    let fields: Vec<_> = argv.strip_suffix(&[0])?.split(|byte| *byte == 0).collect();
    let role = match fields.as_slice() {
        [
            _,
            b"--config",
            config,
            b"--trace",
            b"/data/local/echo/private/bridge.trace",
        ] if *config == CONFIG.as_bytes() => Role::Core,
        [_, b"device", b"-P", b"wyoming", b"-D", b"-E"] => Role::Device,
        _ => return None,
    };
    let status = fs::read_to_string(path.join("status")).ok()?;
    let identity = role.identity();
    let ids = |name: &str| -> Option<Vec<u32>> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))?
            .split_ascii_whitespace()
            .map(|value| value.parse().ok())
            .collect()
    };
    let gids = [
        identity.real_gid,
        identity.effective_gid,
        identity.effective_gid,
        identity.effective_gid,
    ];
    let mut groups = identity.groups.to_vec();
    groups.sort_unstable();
    let correct = ids("Uid:").as_deref() == Some(&[identity.uid; 4])
        && ids("Gid:").as_deref() == Some(&gids)
        && ids("Groups:").as_deref() == Some(&groups)
        && identity.domain.is_none_or(|expected| {
            fs::read_to_string(path.join("attr/current"))
                .is_ok_and(|context| context.trim_end_matches(['\0', '\n']) == expected)
        });
    Some((role, correct))
}

pub(super) fn doctor(files: &Files<'_>) -> Result<String> {
    let (core, device) = roles(files)?;
    let status = |passed| if passed { "ok" } else { "failed" };
    let epoch = SystemClock
        .now()
        .wall
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "read Athena clock")?
        .as_secs();
    let free = rustix::fs::statvfs(layout::ROOT).map_err(|_| "read Athena disk")?;
    let space = free.f_bavail.saturating_mul(free.f_frsize) / 1024;
    let egress = child_succeeded("egress", Duration::from_secs(7))?;
    let user = if core == Some(true) { "nobody" } else { "unavailable" };
    Ok(format!(
        "hassmic={}\negress={}\nclock={epoch}\ndisk_free={space}\nprocess_user={user}",
        status(device),
        status(egress)
    ))
}

pub(super) fn cursor(files: &Files<'_>) -> Result<String> {
    let mut source = private::read(files, layout::TRACE)?;
    let meta = source.metadata().map_err(|_| "inspect wake trace")?;
    let (device, inode) = identity(&meta);
    let length = meta.len();
    let digest = prefix(&mut source, length)?;
    Ok(format!("{device}:{inode}:{length}:{digest}"))
}

pub(super) fn confirm(files: &Files<'_>, cursor: &str) -> Result<&'static str> {
    let mut parts = cursor.split(':');
    let number = |part: Option<&str>| {
        part.and_then(|part| part.parse::<u64>().ok())
            .ok_or("invalid wake cursor")
    };
    let wanted = (number(parts.next())?, number(parts.next())?);
    let length = number(parts.next())?;
    let digest = parts.next().ok_or("invalid wake cursor")?;
    if parts.next().is_some() {
        return Err("invalid wake cursor");
    }
    let mut trace = private::read(files, layout::TRACE)?;
    let meta = trace.metadata().map_err(|_| "inspect wake trace")?;
    if identity(&meta) != wanted || prefix(&mut trace, length)? != digest {
        return Err("wake trace changed");
    }
    let candidate = run_evidence(trace.try_clone().map_err(|_| "read wake trace")?)?;
    if prefix(&mut trace, length)? != digest
        || private::read(files, layout::TRACE)?
            .metadata()
            .map_or(true, |meta| identity(&meta) != wanted)
    {
        return Err("wake trace changed");
    }
    let (core, device) = roles(files)?;
    Ok(if candidate && core == Some(true) && device {
        READY
    } else {
        PENDING
    })
}

fn identity(meta: &Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

fn prefix(source: &mut File, length: u64) -> Result<String> {
    if length > TRACE_LIMIT {
        return Err("wake trace too large");
    }
    source.seek(SeekFrom::Start(0)).map_err(|_| "read wake trace")?;
    let mut hasher = Sha256::new();
    let mut reader = BufReader::new(source.take(length));
    let mut last = None;
    loop {
        let bytes = reader.fill_buf().map_err(|_| "read wake trace")?;
        if bytes.is_empty() {
            break;
        }
        last = bytes.last().copied();
        hasher.update(bytes);
        let count = bytes.len();
        reader.consume(count);
    }
    if reader.into_inner().limit() != 0 || (length != 0 && last != Some(b'\n')) {
        return Err("incomplete wake trace");
    }
    let mut digest = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(digest, "{byte:02x}").map_err(|_| "encode wake digest")?;
    }
    Ok(digest)
}

#[derive(Deserialize)]
struct Run<'a> {
    event: &'a str,
    run_id: Option<&'a str>,
    provider_auth_mode: Option<&'a str>,
}

impl Run<'_> {
    fn sequence(&self) -> Option<u64> {
        self.run_id?
            .strip_prefix("run-")?
            .parse::<u64>()
            .ok()
            .filter(|id| *id != 0)
    }
}

fn run_evidence(trace: impl Read) -> Result<bool> {
    let mut reader = BufReader::new(trace.take(1024 * 1024));
    let mut frame = Vec::new();
    let mut observing = None;
    let mut found = false;
    while reader.read_until(b'\n', &mut frame).map_err(|_| "read wake trace")? != 0 {
        if !frame.ends_with(b"\n") {
            return Ok(false);
        }
        if let Ok(run) = serde_json::from_slice::<Run<'_>>(&frame)
            && let Some(id) = run.sequence()
            && matches!(run.provider_auth_mode, Some("chatgpt" | "api_key"))
        {
            match run.event {
                "wyoming.run_started" => observing = Some(id),
                "run.started" => (),
                // This event follows both run-start frames for endpoint sessions.
                "endpoint.session_started" if observing == Some(id) => observing = None,
                _ if observing == Some(id) => found = true,
                _ => (),
            }
        }
        frame.clear();
    }
    Ok(found && reader.into_inner().limit() != 0)
}

#[cfg(test)]
mod tests {
    use super::run_evidence;
    use std::fmt::Write as _;

    #[test]
    fn a2_wake_requires_a_complete_local_run_and_rejects_remote_or_stale_runs() {
        let mut trace = String::new();
        let record = |trace: &mut String, event: &str, id: u8| {
            writeln!(
                trace,
                r#"{{"event":"{event}","run_id":"run-{id}","provider_auth_mode":"chatgpt"}}"#
            )
            .unwrap();
        };
        record(&mut trace, "wyoming.run_started", 1);
        record(&mut trace, "run.started", 1);
        assert_eq!(run_evidence(trace.as_bytes()), Ok(false));
        record(&mut trace, "endpoint.session_started", 1);
        record(&mut trace, "mic.completed", 1);
        assert_eq!(run_evidence(trace.as_bytes()), Ok(false));
        let baseline = trace.len();
        record(&mut trace, "mic.completed", 2);
        assert_eq!(run_evidence(&trace.as_bytes()[baseline..]), Ok(false));
        record(&mut trace, "wyoming.run_started", 3);
        record(&mut trace, "run.started", 3);
        record(&mut trace, "mic.completed", 3);
        assert_eq!(run_evidence(&trace.as_bytes()[baseline..]), Ok(true));
        trace.push('{');
        assert_eq!(run_evidence(&trace.as_bytes()[baseline..]), Ok(false));
    }
}
