use super::*;

async fn journey(negative: bool) {
    let root = std::env::temp_dir().join(format!("lab-m4-host-{}-{}", std::process::id(), negative));
    let bridge = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(if cfg!(windows) {
            "echo-bridge.exe"
        } else {
            "echo-bridge"
        });
    let mut world = World::default();
    let work = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        world
            .start(
                if cfg!(windows) { "python" } else { "python3" },
                &bridge,
                &root,
                negative,
            )
            .await?;
        let (completion, timing) = world.speak().await?;
        let observer = world.observer()?;
        observer
            .wait_for(0, |call| call.reply_completed)
            .await
            .map_err(|_| "m4: reply not completed")?;
        world.check_peers()?;
        Ok::<_, String>((completion, timing, observer))
    })
    .await;
    let cleanup = world.close().await;
    assert_eq!(
        cleanup,
        Ok(()),
        "work error: {:?}; trace: {:?}",
        work.as_ref().map(|r| r.as_ref().err()),
        world.trace.records
    );
    assert!(!root.exists());
    let (completion, timing, observer) = work.unwrap().unwrap();
    assert_eq!(completion, json!({"event":"bridge.run_completed","run_id":"run-1"}));
    assert_eq!(timing.captured, fixture().len() + 25600);
    assert_eq!(timing.played, 9600);
    assert!(timing.sample().unwrap() <= std::time::Duration::from_millis(1500));
    super::super::tests::assert_dialogue(&observer, if negative { FALLBACK } else { SUCCESS });
    let selectors = super::super::super::selectors::parse(&[
        json!("session.delegation.created"),
        json!("tools/call {name: test.echo, count: 1}"),
        json!("session.commentary.append"),
    ])
    .unwrap();
    assert_eq!(
        super::super::super::selectors::check(&world.trace.records, &selectors, Some("run-1")),
        Ok(())
    );
    assert_eq!(world.close().await, Ok(()));
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt echo-bridge"]
async fn t3_m4_pinned_host_positive_finite_capture_screen_reply_and_cleanup() {
    journey(false).await;
}

#[tokio::test]
#[ignore = "T3 requires pinned hassmic-host and prebuilt echo-bridge"]
async fn t3_m4_pinned_host_http500_finite_capture_screen_fallback_and_cleanup() {
    journey(true).await;
}

#[tokio::test]
async fn destination_collision_fails_before_startup_and_preserves_previous_owner() {
    let root = std::env::temp_dir().join(format!("lab-m4-existing-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("retained"), "previous owner").unwrap();
    let mut world = World::default();
    assert_eq!(
        world.start("must-not-start", Path::new("missing"), &root, false).await,
        Err("m4: cannot own private directory".into())
    );
    assert_eq!(world.close().await, Ok(()));
    assert_eq!(
        std::fs::read_to_string(root.join("retained")).unwrap(),
        "previous owner"
    );
    std::fs::remove_file(root.join("retained")).unwrap();
    std::fs::remove_dir(root).unwrap();
}
