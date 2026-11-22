use std::{
    ffi::{OsStr, OsString},
    os::unix::net::SocketAddr,
    path::PathBuf,
};

const SOCKET_OPTION: &str = "--wyoming-socket";

pub(super) fn select(
    args: impl IntoIterator<Item = OsString>,
    android: bool,
) -> Result<(Vec<OsString>, Option<PathBuf>), &'static str> {
    let mut args = args.into_iter();
    let mut forwarded = Vec::new();
    let mut socket = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            forwarded.push(arg);
            forwarded.extend(args);
            break;
        }
        if arg == SOCKET_OPTION {
            if socket.is_some() {
                return Err("--wyoming-socket must appear at most once");
            }
            let path = args
                .next()
                .map(PathBuf::from)
                .ok_or("--wyoming-socket requires a path")?;
            // Path::file_name normalizes a terminal dot. The kernel sees the raw path.
            let name = path.as_os_str().as_encoded_bytes().rsplit(|byte| *byte == b'/').next();
            if !path.is_absolute()
                || matches!(name, Some(b"" | b"." | b".."))
                || SocketAddr::from_pathname(&path).is_err()
            {
                return Err("--wyoming-socket requires an absolute filesystem socket path");
            }
            socket = Some(path);
        } else {
            let takes_value = takes_separate_value(&arg);
            forwarded.push(arg);
            if takes_value {
                forwarded.extend(args.next());
            }
        }
    }
    Ok((
        forwarded,
        socket.or_else(|| android.then(|| super::WYOMING_SOCKET.into())),
    ))
}

// Preserve the native short options, including attached values and clusters.
fn takes_separate_value(arg: &OsStr) -> bool {
    arg.as_encoded_bytes()
        .strip_prefix(b"-")
        .filter(|flags| !flags.starts_with(b"-"))
        .and_then(|flags| {
            flags
                .iter()
                .position(|flag| b"PpnwmbzoaiWG".contains(flag))
                .map(|i| (i, flags.len()))
        })
        .is_some_and(|(index, len)| index + 1 == len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport(args: &[&str], android: bool) -> (Vec<OsString>, Option<PathBuf>) {
        select(args.iter().map(OsString::from), android).unwrap()
    }

    #[test]
    fn socket_policy_keeps_supervisor_argv_and_linux_defaults() {
        let options = ["-P", "wyoming", "-D", "-p", "16700", "-S"];
        assert_eq!(
            transport(&options, true),
            (
                options.map(OsString::from).to_vec(),
                Some(super::super::WYOMING_SOCKET.into())
            )
        );
        assert_eq!(transport(&options, false), (options.map(OsString::from).to_vec(), None));
        for android in [false, true] {
            assert_eq!(
                transport(&["-Dp16700", SOCKET_OPTION, "/tmp/sat.sock"], android),
                (vec!["-Dp16700".into()], Some("/tmp/sat.sock".into()))
            );
        }
        assert_eq!(
            transport(&["-Dn", SOCKET_OPTION], false),
            (vec!["-Dn".into(), SOCKET_OPTION.into()], None)
        );
    }

    #[test]
    fn socket_policy_rejects_invalid_or_repeated_options() {
        let too_long = format!("/{}", "x".repeat(107));
        for args in [
            vec![SOCKET_OPTION],
            vec![SOCKET_OPTION, "relative"],
            vec![SOCKET_OPTION, "/"],
            vec![SOCKET_OPTION, "/tmp/socket/"],
            vec![SOCKET_OPTION, "/tmp/."],
            vec![SOCKET_OPTION, "/tmp/.."],
            vec![SOCKET_OPTION, "/tmp/\0socket"],
            vec![SOCKET_OPTION, &too_long],
            vec![SOCKET_OPTION, "/tmp/one", SOCKET_OPTION, "/tmp/two"],
        ] {
            assert!(select(args.into_iter().map(OsString::from), false).is_err());
        }
    }
}
