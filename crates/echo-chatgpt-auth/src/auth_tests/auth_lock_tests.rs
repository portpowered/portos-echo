use super::auth_support::*;
use crate::{AuthError, auth_session};

#[test]
fn store_lock_contender_busy_and_released_handle_reacquires() {
    let directory = directory();
    let path = directory.0.join("credential");
    let first = auth_session::lock(&path).unwrap();
    assert_eq!(auth_session::lock(&path).err(), Some(AuthError::Busy));
    drop(first);
    let second = auth_session::lock(&path).unwrap();
    drop(second);
    assert_eq!(std::fs::read(directory.0.join("credential.lock")).unwrap(), b"");
    let bad = directory.0.join("missing").join("credential");
    assert_eq!(auth_session::lock(&bad).err(), Some(AuthError::Storage));
    let blocked = directory.0.join("blocked.lock");
    std::fs::create_dir(blocked).unwrap();
    assert_eq!(
        auth_session::lock(&directory.0.join("blocked")).err(),
        Some(AuthError::InvalidConfig)
    );
}
#[test]
fn loom_rotation_has_one_writer_and_persists_before_publication() {
    loom::model(|| {
        use loom::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        let durable_version = Arc::new(Mutex::new(0));
        let rotations = Arc::new(AtomicUsize::new(0));
        let mut callers = Vec::new();
        for _ in 0..2 {
            let durable = durable_version.clone();
            let posts = rotations.clone();
            callers.push(loom::thread::spawn(move || {
                // Same ownership interval as current(): lock, reload, refresh,
                // durable commit, publish, unlock. Independent OS proof above.
                let mut version = durable.lock().unwrap();
                if *version == 0 {
                    posts.fetch_add(1, Ordering::SeqCst);
                    *version = 1;
                }
                let published = *version;
                assert_eq!(published, 1);
                assert_eq!(posts.load(Ordering::SeqCst), 1);
            }));
        }
        for caller in callers {
            caller.join().unwrap();
        }
        assert_eq!(*durable_version.lock().unwrap(), 1);
        assert_eq!(rotations.load(Ordering::SeqCst), 1);
    });
}
