//! T3: drive the native orchestration with a verified synthetic policy and
//! the same process/loopback boundaries as the native CLI fixture.
use super::*;

#[path = "service.rs"]
mod service;

type Test = std::result::Result<(), Box<dyn std::error::Error>>;

fn fixture(options: &[&str]) -> std::result::Result<(Scratch, Options), Box<dyn std::error::Error>> {
    let file = format!("echo-setup-test-adb{}", std::env::consts::EXE_SUFFIX);
    let test = std::env::current_exe()?;
    let executable = test
        .ancestors()
        .map(|parent| parent.join(&file))
        .find(|path| path.is_file())
        .ok_or("Cargo-built fake adb missing")?;
    let root = Scratch::create().map_err(|_| "fixture root")?;
    let adb = root.0.join(file);
    fs::copy(executable, &adb)?;
    let release = root.0.join("bundle");
    let mut sums = std::collections::BTreeMap::new();
    for name in integrity::REQUIRED {
        let member = release.join(name);
        fs::create_dir_all(member.parent().ok_or("bundle parent missing")?)?;
        fs::write(member, b"public fixture")?;
        sums.insert(name, integrity::digest(b"public fixture"));
    }
    fs::write(
        release.join("MANIFEST"),
        serde_json::to_vec(&serde_json::json!({"format":1,"version":"0.2.0","files":sums}))?,
    )?;
    fs::write(root.0.join("devices"), "SYNTHETIC-DEVICE\tdevice\n")?;
    let args = ["install", "--install-path", "a2", "--adb"]
        .into_iter()
        .map(std::ffi::OsString::from)
        .chain([adb.into_os_string(), "--from-dir".into(), release.into_os_string()])
        .chain(options.iter().map(std::ffi::OsString::from));
    let options = Options::parse(args).map_err(|_| "invalid fixture invocation")?;
    Ok((root, options))
}

fn ready(root: &Scratch) -> std::result::Result<service::Service, Box<dyn std::error::Error>> {
    let service = service::Service::new(&root.0)?;
    let config =
        include_str!("../../../../examples/bridge.echo.toml").replace("/data/local/echo-bridge/", "/data/local/echo/");
    fs::write(root.0.join("setup-config"), config)?;
    let clock = std::time::UNIX_EPOCH.elapsed()?.as_secs();
    fs::write(
        root.0.join("doctor"),
        format!("hassmic=ok\negress=ok\nclock={clock}\ndisk_free=262144\nprocess_user=nobody\n"),
    )?;
    fs::write(root.0.join("link-state"), "linked")?;
    Ok(service)
}

#[tokio::test]
async fn t3_a2_install_stages_bare_root_reboots_and_confirms_verified_version() -> Test {
    let (root, options) = fixture(&["--timeout-seconds", "8"])?;
    let _service = ready(&root)?;
    let mut instructions = Vec::new();
    let actual = install_with_policy(
        &options,
        |_| Err(Failure("unexpected_prompt")),
        || Err(Failure("legacy_wake")),
        |step| {
            instructions.push(step.to_owned());
            Ok(())
        },
        &crate::Cancellation::default(),
        async { policy::tests::fixture(true, true) },
    )
    .await;
    assert_eq!(actual, Ok("0.2.0".to_owned()));
    assert_eq!(
        instructions.iter().map(|s| s.as_bytes()[0]).collect::<Vec<_>>(),
        b"1234"
    );
    let commands: Vec<Vec<String>> = fs::read_to_string(root.0.join("ledger"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(commands[0], ["devices"]);
    assert!(commands[1..].iter().all(|args| args[..2] == ["-s", "SYNTHETIC-DEVICE"]));
    let calls: Vec<_> = commands.iter().filter_map(|args| args.last()).collect();
    let index = |expected: &str| {
        calls
            .iter()
            .position(|call| call.contains(expected))
            .ok_or("expected command missing")
    };
    assert!(index("id -ru && id -u")? < index("/magiskpolicy")?);
    assert!(index("--host-stage --no-reboot")? < index("rm -rf '/data/local/echo-setup-")?);
    let reboot = calls
        .iter()
        .position(|call| call.as_str() == "reboot")
        .ok_or("no reboot request")?;
    assert!(index("rm -rf '/data/local/echo-setup-")? < reboot);
    assert!(reboot < index("getprop sys.boot_completed")?);
    assert!(index("getprop sys.boot_completed")? < index("--host status")?);
    assert!(index("--host status")? < index("--host ensure-signin")?);
    assert!(index("--host confirm-check")? < index("--host doctor")?);
    assert_eq!(fs::read_to_string(root.0.join("forwards.json"))?, "[]");
    Ok(())
}

#[tokio::test]
async fn t3_a2_install_rejects_both_policy_digests_before_any_adb_invocation() -> Test {
    for (archive, diagnostic) in [
        (false, "magiskpolicy_archive_checksum_mismatch"),
        (true, "magiskpolicy_checksum_mismatch"),
    ] {
        let (root, options) = fixture(&[])?;
        let result = install_with_policy(
            &options,
            |_| Ok(()),
            || Ok(()),
            |_| Ok(()),
            &crate::Cancellation::default(),
            async { policy::tests::fixture(archive, false) },
        )
        .await;
        assert_eq!(result, Err(Failure(diagnostic)));
        assert!(!root.0.join("ledger").exists());
    }
    Ok(())
}
