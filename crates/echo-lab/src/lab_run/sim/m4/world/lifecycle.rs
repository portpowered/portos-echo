//! T3 lifecycle on the canonical real-pin runner and native hassmic-host.
use super::*;
use crate::lab_run::sim::process::owners::Host;
use std::path::PathBuf;

pub(super) struct Owners {
    host: Host,
    host_child: std::sync::mpsc::Receiver<std::process::ExitStatus>,
    bridge_child: std::sync::mpsc::Receiver<std::process::ExitStatus>,
    pub root: PathBuf,
    addresses: Vec<String>,
}
impl Owners {
    pub async fn closed(self) {
        assert!(self.host_child.try_recv().unwrap().success(), "host not collected");
        assert!(self.bridge_child.try_recv().unwrap().success(), "bridge not collected");
        self.host.assert_closed();
        assert!(!self.root.exists());
        let mut checks = tokio::task::JoinSet::new();
        for address in self.addresses {
            checks.spawn(async move {
                assert!(tokio::net::TcpStream::connect(address).await.is_err());
            });
        }
        while let Some(result) = checks.join_next().await {
            result.unwrap();
        }
    }
}

pub(super) async fn start(emotion: bool, negative: bool) -> (Case, Owners) {
    let host = Host::new();
    let options = tests::options();
    let mut case = Case {
        emotion,
        ..Case::default()
    };
    case.start_host(&options, negative, host.python()).await.unwrap();
    let root = case.root.as_ref().unwrap().0.clone();
    let raw = std::fs::read_to_string(root.join("trace")).unwrap();
    let mut addresses: Vec<String> = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|row| row["address"].as_str().map(str::to_owned))
        .collect();
    for origin in [
        &case.provider.as_ref().unwrap().origin,
        case.spoof.as_ref().unwrap().url(),
    ] {
        addresses.push(
            origin
                .strip_prefix("http://")
                .unwrap()
                .split('/')
                .next()
                .unwrap()
                .into(),
        );
    }
    let owners = Owners {
        host,
        host_child: case.host.as_mut().unwrap().observe_collection(),
        bridge_child: case.bridge.as_mut().unwrap().observe_collection(),
        root,
        addresses,
    };
    (case, owners)
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host, prebuilt bridge and offline Codex pin"]
async fn t3_m4_cancel_active_reply_reaps_children_listeners_and_private_files() {
    let (mut case, owners) = start(false, false).await;
    let mut screen = Screen::connect(&owners.root).await.unwrap();
    screen.expect("ready").await.unwrap();
    let cancel = owners.root.join("cancel");
    let cancelled = async {
        for state in ["listening", "connected", "speaking"] {
            screen.expect(state).await.unwrap();
        }
        std::fs::write(&cancel, "cancel").unwrap();
    };
    let (result, ()) = tokio::join!(
        controlled(
            case.speak(),
            Some(&cancel),
            tokio::time::Instant::now() + Duration::from_secs(20)
        ),
        cancelled
    );
    assert_eq!(result.err(), Some("sim: cancelled".into()));
    let closed = case.close().await;
    // The full bounded reply and transcript are collected before its mouth
    // opens. Cancellation still forbids claiming a settled played reply.
    assert_eq!(closed, Err("m4: missing played reply observation".into()));
    assert_eq!(case.snapshot, receipts::expected(false, false));
    assert!(!case.trace.records.iter().any(|r| r["event"] == "reply.played"));
    assert!(!case.trace.records.iter().any(|r| r["event"] == "bridge.run_completed"));
    owners.closed().await;
    assert_eq!(case.close().await, Ok(()));
}

#[tokio::test]
async fn cleanup_replaced_directory_is_reported() {
    let root = Root::new().unwrap();
    std::fs::remove_dir(&root.0).unwrap();
    std::fs::write(&root.0, "replacement").unwrap();
    assert_eq!(
        remove_root(&root.0),
        Err("cleanup: private M4 directory removal failed".into())
    );
    assert_eq!(std::fs::read_to_string(&root.0).unwrap(), "replacement");
    std::fs::remove_file(&root.0).unwrap();
    assert_eq!(remove_root(&root.0), Ok(()));
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host, prebuilt bridge and offline Codex pin"]
async fn t3_m4_parallel_world_failure_does_not_borrow_success_or_leave_listeners() {
    let ((mut failed, a), (mut passed, b)) = tokio::join!(start(false, false), start(false, false));
    assert_ne!(a.root, b.root);
    assert!(a.addresses.iter().all(|address| !b.addresses.contains(address)));
    failed.provider.take().unwrap().close().await.unwrap();
    let (failure, success) = tokio::join!(failed.speak(), passed.speak());
    assert!(failure.is_err());
    assert!(failed.speaking.is_none());
    assert_eq!(success.unwrap().0["run_id"], "run-1");
    let (failure, success) = tokio::join!(failed.close(), passed.close());
    assert!(
        failure
            .unwrap_err()
            .contains("missing or duplicate native delegation.created")
    );
    assert_eq!(success, Ok(()));
    tokio::join!(a.closed(), b.closed());
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host, prebuilt bridge and offline Codex pin"]
async fn t3_m4_teardown_failure_surfaces_after_collecting_all_peers() {
    let (mut case, owners) = start(false, false).await;
    assert_eq!(case.speak().await.unwrap().0["run_id"], "run-1");
    case.provider.as_ref().unwrap().quiesce().await.unwrap();
    let mut failures = vec![];
    case.close_bridge(&mut failures).await;
    assert_eq!(failures, Vec::<String>::new());
    let retained = owners.root.with_extension("retained");
    std::fs::rename(&owners.root, &retained).unwrap();
    std::fs::write(&owners.root, "replacement").unwrap();
    let closed = case.close().await.unwrap_err();
    assert!(
        closed.contains("cleanup: private M4 directory removal failed"),
        "{closed}"
    );
    assert_eq!(std::fs::read_to_string(&owners.root).unwrap(), "replacement");
    std::fs::remove_file(&owners.root).unwrap();
    std::fs::remove_dir_all(retained).unwrap();
    owners.closed().await;
    assert_eq!(case.close().await, Ok(()));
}
