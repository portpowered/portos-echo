use super::probe_command::command;
use std::path::Path;
use std::process::Output;

pub const READS: &[&[&str]] = &[
    &["getprop", "ro.serialno"],
    &["getprop", "ro.boot.serialno"],
    &["getprop", "ro.product.device"],
    &["getprop", "ro.build.version.incremental"],
    &["getprop", "ro.build.version.sdk"],
    &["getprop", "ro.build.fingerprint"],
    &["uname", "-r"],
    &["md5sum", "/system/lib/libpryon.so"],
    &["md5sum", "/system/vendor/lib/libasp.so"],
    &["md5sum", "/system/lib/libaspclient.so"],
    &["md5sum", "/system/lib/libaudioflinger.so"],
    &["md5sum", "/system/vendor/lib/hw/audio.primary.mt8183.so"],
    &["md5sum", "/system/lib/libwakewordserver_jni.so"],
    &["getevent", "-lp"],
    &["cat", "/proc/bus/input/devices"],
    &["tinymix"],
    &["dumpsys", "input"],
    &["wm", "size"],
    &["wm", "density"],
    &["ls", "/system/local/models/keyword"],
];

pub struct Observation {
    pub stdout: String,
    pub stderr: String,
}

pub enum Collection {
    Refused(String),
    Athena {
        raw: Vec<Observation>,
        serials: Vec<String>,
    },
}

pub fn scrub(text: &str, serials: &[String]) -> String {
    let mut needles: Vec<_> = serials.iter().filter(|s| !s.is_empty()).collect();
    needles.sort_by_key(|s| std::cmp::Reverse(s.len()));
    // Match against original input so replacements can never expose a suffix
    // of an overlapping identifier or replace text inside our marker.
    let mut clean = String::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if let Some(needle) = needles.iter().find(|s| remaining.starts_with(s.as_str())) {
            clean.push_str("<serial>");
            remaining = &remaining[needle.len()..];
        } else if let Some(character) = remaining.chars().next() {
            clean.push(character);
            remaining = &remaining[character.len_utf8()..];
        }
    }
    clean
}

fn invoke(adb: &Path, read: &[&str]) -> Result<Output, String> {
    command(adb, None, read)
        .output()
        .map_err(|_| format!("probe {}: cannot launch explicit adb", read.join(" ")))
}

fn discover(adb: &Path) -> Result<(Vec<String>, Vec<Output>), String> {
    let mut serials = Vec::new();
    let mut raw = Vec::new();
    for read in &READS[..2] {
        let output = invoke(adb, read).map_err(|_| "probe serial discovery failed; child output discarded")?;
        if !output.status.success() {
            return Err("probe serial discovery failed; child output discarded".into());
        }
        let serial = std::str::from_utf8(&output.stdout)
            .map_err(|_| "probe serial discovery failed; child output discarded")?
            .trim();
        if serial.chars().any(char::is_whitespace) {
            return Err("probe serial discovery failed; child output discarded".into());
        }
        serials.push(serial.to_owned());
        raw.push(output);
    }
    Ok((serials, raw))
}

fn sanitize(output: &Output, serials: &[String]) -> Observation {
    Observation {
        stdout: scrub(&String::from_utf8_lossy(&output.stdout), serials),
        stderr: scrub(&String::from_utf8_lossy(&output.stderr), serials),
    }
}

pub fn collect(adb: &Path) -> Result<Collection, String> {
    let (serials, discovery) = discover(adb)?;
    let mut raw: Vec<_> = discovery.iter().map(|o| sanitize(o, &serials)).collect();
    for read in &READS[2..] {
        let result = invoke(adb, read).map_err(|e| scrub(&e, &serials))?;
        let clean = sanitize(&result, &serials);
        if !result.status.success() {
            return Err(scrub(
                &format!("probe {}: read failed: {}", read.join(" "), clean.stderr.trim()),
                &serials,
            ));
        }
        if raw.len() == 2 && clean.stdout.trim() != "athena" {
            return Ok(Collection::Refused(clean.stdout.trim().to_owned()));
        }
        raw.push(clean);
    }
    Ok(Collection::Athena { raw, serials })
}
