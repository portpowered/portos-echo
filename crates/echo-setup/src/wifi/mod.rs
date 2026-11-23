//! USB recovery through the A2 device role. Credentials only cross stdin.
use crate::{Failure, Result, adb::Adb, cli};
use echo_contracts::secret::Secret;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, BufRead, BufReader, Write},
    path::PathBuf,
    time::Duration,
};

const DEVICE: &str = "/data/local/echo/bin/current";
const UNSUPPORTED: Failure = Failure("wifi-unsupported");
const DEVICE_FAILED: Failure = Failure("wifi-device-failed");
const INPUT_FAILED: Failure = Failure("wifi-input-failed");
const TIMEOUT: Failure = Failure("wifi-timeout");

enum Request {
    Status,
    Join { ssid: String, file: Option<PathBuf> },
}

impl Request {
    fn operation(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Join { .. } => "join",
        }
    }
}

fn parse(mut args: impl Iterator<Item = OsString>) -> Result<(Request, cli::Options)> {
    let operation = args.next();
    let join = match operation.as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("status") => false,
        Some("join") => true,
        _ => return Err(Failure("usage")),
    };
    let mut ssid = None;
    let mut file = None;
    let mut common = vec![OsString::from("wifi")];
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--json") => common.push(flag),
            Some("--adb" | "--serial" | "--timeout-seconds") => {
                common.extend([flag, value(args.next())?]);
            }
            Some("--ssid") if join && ssid.is_none() => {
                let name = value(args.next())?.into_string().map_err(|_| Failure("usage"))?;
                if !valid_ssid(&name) {
                    return Err(Failure("usage"));
                }
                ssid = Some(name);
            }
            Some("--password-file") if join && file.is_none() => file = Some(value(args.next())?.into()),
            _ => return Err(Failure("usage")),
        }
    }
    let request = if join {
        Request::Join {
            ssid: ssid.ok_or(Failure("usage"))?,
            file,
        }
    } else {
        Request::Status
    };
    Ok((request, cli::Options::parse(common)?))
}

fn value(arg: Option<OsString>) -> Result<OsString> {
    arg.filter(|arg| !arg.is_empty() && !arg.to_string_lossy().starts_with("--"))
        .ok_or(Failure("usage"))
}

/// Parse the Wi-Fi command, run it and render the fixed result.
///
/// # Errors
/// Returns a usage error before running adb or reading a credential.
pub async fn run(args: impl Iterator<Item = OsString>) -> Result<i32> {
    let (request, options) = parse(args)?;
    let op = request.operation();
    let result = tokio::time::timeout(Duration::from_secs(options.timeout_seconds), execute(request, &options))
        .await
        .unwrap_or(Err(TIMEOUT))
        .unwrap_or_else(Reply::failed);
    result.render(op, options.json);
    Ok(match result.error {
        None => 0,
        Some(TIMEOUT) => 3,
        Some(_) => 1,
    })
}

async fn execute(request: Request, options: &cli::Options) -> Result<Reply> {
    let op = request.operation();
    let mut adb = Adb::discover(options.adb.as_deref())?;
    adb.select_ready(options.serial.as_deref()).await.map_err(adb_error)?;
    let (target, password) = match request {
        Request::Status => (None, None),
        Request::Join { ssid, file } => (Some(ssid), Some(read_password(file).await?)),
    };
    let input = password.as_ref().map(|password| {
        let data = json!({"version":1, "op":op, "ssid":target, "password":password.expose()});
        Secret::new(format!("{}\n", json!({"type":"wifi-request", "data":data})))
    });
    let remote =
        format!("if [ ! -e {DEVICE} ]; then printf '%s\\n' wifi-unsupported; exit 1; fi; exec {DEVICE} wifi {op}");
    let (success, output) = adb
        .wifi(&remote, input.as_ref().map(|input| input.expose().as_bytes()))
        .await
        .map_err(adb_error)?;
    let mut reply = parse_reply(success, &output, op, target.as_deref())?;
    if let (Some(status), Some(password)) = (&mut reply.status, password.as_ref()) {
        let password = password.expose();
        if !password.is_empty() && status.ssid.as_ref().is_some_and(|name| name.contains(password)) {
            status.ssid = None;
        }
    }
    Ok(reply)
}

fn adb_error(error: Failure) -> Failure {
    match error.0 {
        "adb_timeout" => TIMEOUT,
        "adb_failed" | "adb_output_limit" => DEVICE_FAILED,
        _ => error,
    }
}

async fn read_password(file: Option<PathBuf>) -> Result<Secret<String>> {
    // A waiting terminal or pipe must not keep the CLI alive after its deadline.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .spawn(move || {
            let password = match file {
                Some(path) => File::open(path)
                    .map_err(|_| INPUT_FAILED)
                    .and_then(|file| line(BufReader::new(file))),
                None => line(io::stdin().lock()),
            };
            let _ = sender.send(password);
        })
        .map_err(|_| INPUT_FAILED)?;
    receiver.await.map_err(|_| INPUT_FAILED)?
}

fn line(reader: impl BufRead) -> Result<Secret<String>> {
    let mut bytes = Vec::new();
    let count = reader
        .take(67)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| INPUT_FAILED)?;
    if count == 0 {
        return Err(INPUT_FAILED);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > 64 || bytes.contains(&0) {
        return Err(INPUT_FAILED);
    }
    String::from_utf8(bytes).map(Secret::new).map_err(|_| INPUT_FAILED)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    #[serde(rename = "type")]
    kind: String,
    data: Data,
}

#[derive(Deserialize)]
struct Data {
    version: u32,
    op: String,
    ok: bool,
    #[serde(flatten)]
    fields: Map<String, Value>,
}

struct Reply {
    error: Option<Failure>,
    status: Option<Status>,
}

struct Status {
    ssid: Option<String>,
    reachable: bool,
}

fn valid_ssid(name: &str) -> bool {
    (1..=32).contains(&name.len()) && !name.contains('\0')
}

fn parse_reply(success: bool, output: &str, op: &str, target: Option<&str>) -> Result<Reply> {
    if !success && output == "wifi-unsupported\n" {
        return Err(UNSUPPORTED);
    }
    let line = output
        .strip_suffix('\n')
        .filter(|line| !line.contains('\n'))
        .ok_or(DEVICE_FAILED)?;
    let Frame { kind, mut data } = serde_json::from_str(line).map_err(|_| DEVICE_FAILED)?;
    if kind != "wifi-result" || data.version != 1 || data.op != op || data.ok != success {
        return Err(DEVICE_FAILED);
    }
    if success {
        let status = status(data.fields)?;
        if target.is_some_and(|ssid| !status.reachable || status.ssid.as_deref() != Some(ssid)) {
            return Err(DEVICE_FAILED);
        }
        return Ok(Reply {
            error: None,
            status: Some(status),
        });
    }
    let error = data.fields.remove("error");
    match (error.as_ref().and_then(Value::as_str), target) {
        (Some("wifi-unsupported"), _) if data.fields.is_empty() => Err(UNSUPPORTED),
        (Some("wifi-unavailable"), _) if data.fields.is_empty() => Err(Failure("wifi-unavailable")),
        (Some("wifi-busy"), Some(_)) if data.fields.is_empty() => Err(Failure("wifi-busy")),
        (Some("wifi-join-failed"), Some(_)) => rejected(Failure("wifi-join-failed"), data.fields),
        (Some("wifi-rollback-failed"), Some(_)) => rejected(Failure("wifi-rollback-failed"), data.fields),
        _ => Err(DEVICE_FAILED),
    }
}

fn rejected(error: Failure, fields: Map<String, Value>) -> Result<Reply> {
    Ok(Reply {
        error: Some(error),
        status: Some(status(fields)?),
    })
}

fn status(mut fields: Map<String, Value>) -> Result<Status> {
    let reachable = fields
        .remove("reachable")
        .and_then(|value| value.as_bool())
        .ok_or(DEVICE_FAILED)?;
    let ssid = match fields.remove("ssid") {
        Some(Value::String(name)) if valid_ssid(&name) => Some(name),
        Some(Value::Null) if !reachable => None,
        _ => return Err(DEVICE_FAILED),
    };
    if !fields.is_empty() {
        return Err(DEVICE_FAILED);
    }
    Ok(Status { ssid, reachable })
}

impl Reply {
    fn failed(error: Failure) -> Self {
        Self {
            error: Some(error),
            status: None,
        }
    }

    fn render(&self, operation: &str, json_output: bool) {
        if json_output {
            let mut result = json!({
                "type":"result", "command":"wifi", "operation":operation, "ok":self.error.is_none()
            });
            if let Some(error) = self.error {
                result["error"] = json!(error.0);
            }
            if let Some(status) = &self.status {
                result["ssid"] = json!(status.ssid);
                result["reachable"] = json!(status.reachable);
            }
            let _ = writeln!(io::stdout(), "{result}");
        } else if let Some(error) = self.error {
            let _ = writeln!(io::stderr(), "echo-setup: {}.", error.0);
        } else if let Some(status) = &self.status {
            status.render();
        }
    }
}

impl Status {
    fn render(&self) {
        let mut out = io::stdout().lock();
        let reachability = if self.reachable { "reachable" } else { "unreachable" };
        if let Some(ssid) = &self.ssid {
            let mut name = String::new();
            for character in ssid.chars() {
                if character.is_control() {
                    name.extend(character.escape_default());
                } else {
                    name.push(character);
                }
            }
            let _ = writeln!(out, "Wi-Fi: {name} ({reachability})");
        } else {
            let state = if self.reachable { "reachable" } else { "disconnected" };
            let _ = writeln!(out, "Wi-Fi: {state}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_password_line_keeps_spaces_supports_open_networks_and_refuses_overflow() -> Result<()> {
        for (bytes, expected) in [
            (&b" spaces \r\nunused"[..], " spaces "),
            (&b"\n"[..], ""),
            (&b"last line"[..], "last line"),
        ] {
            assert_eq!(line(bytes)?.expose(), expected);
        }
        for bytes in [&b""[..], &b"\0\n"[..], &[b'x'; 65][..], &b"\xFF\n"[..]] {
            assert!(matches!(line(bytes), Err(INPUT_FAILED)));
        }
        Ok(())
    }
}
