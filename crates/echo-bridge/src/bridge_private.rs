//! Owner-only creation of the bridge's own state, whatever umask the launcher left.
//! A directory or file that already exists keeps the mode its operator chose.
use std::{
    fs::{DirBuilder, OpenOptions},
    path::Path,
};

/// Create `path` and any missing parents with mode 0700.
pub(crate) fn directory(path: &Path) -> std::io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

/// A file these options create gets mode 0600.
pub(crate) fn file(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(options, 0o600);
    options
}

#[cfg(all(test, unix))]
mod tests {
    use crate::{bridge_runtime::bridge_kill, bridge_trace::Trace};
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    // The test process's umask (022 or 002) would otherwise leave 0755/0644 here;
    // `adb shell` on the Echo runs with umask 000, which left them world-writable.
    #[test]
    fn audit_directory_kill_marker_and_trace_are_created_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let audit = root.path().join("logs/audit");
        let marker = audit.join("bridge-kill");
        bridge_kill::mutate(&marker, true).unwrap();
        assert_eq!(mode(&root.path().join("logs")), 0o700);
        assert_eq!(mode(&audit), 0o700);
        assert_eq!(mode(&marker), 0o600);
        assert_eq!(bridge_kill::inspect(&marker), Ok(true));

        let trace = root.path().join("trace.jsonl");
        Trace::open(&trace, "chatgpt")
            .unwrap()
            .record("run.started", Some("run-1"), serde_json::json!({}))
            .unwrap();
        assert_eq!(mode(&trace), 0o600);
        assert_eq!(std::fs::read_to_string(&trace).unwrap().lines().count(), 1);
    }

    #[test]
    fn existing_state_keeps_the_mode_its_operator_chose() {
        let root = tempfile::tempdir().unwrap();
        let audit = root.path().join("audit");
        std::fs::create_dir(&audit).unwrap();
        std::fs::set_permissions(&audit, std::fs::Permissions::from_mode(0o750)).unwrap();
        let trace = root.path().join("trace.jsonl");
        std::fs::write(&trace, b"").unwrap();
        std::fs::set_permissions(&trace, std::fs::Permissions::from_mode(0o640)).unwrap();
        bridge_kill::mutate(&audit.join("bridge-kill"), true).unwrap();
        Trace::open(&trace, "chatgpt").unwrap();
        assert_eq!((mode(&audit), mode(&trace)), (0o750, 0o640));
        assert_eq!(mode(&audit.join("bridge-kill")), 0o600);
    }
}
