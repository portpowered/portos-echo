//! T3 native config staging through the compiled CLI and recording ADB.
use super::*;
use std::path::Path;

pub(super) fn assert_config_dispatch(ledger: &[Vec<String>], config: &Path, force: bool) -> Result {
    let local = config.to_str().ok_or("fixture path")?;
    let pushes: Vec<_> = ledger
        .iter()
        .filter(|args| args.get(3).is_some_and(|arg| arg == local))
        .collect();
    let [push] = pushes.as_slice() else {
        return Err("expected exactly one config push".into());
    };
    let remote = push[4].as_str();
    assert_eq!(push.as_slice(), ["-s", "SYNTHETIC-DEVICE", "push", local, remote]);
    let stage = remote.strip_suffix("/config").ok_or("missing staged config path")?;
    assert!(stage.starts_with("/data/local/tmp/echo-setup-"));
    let bundle = format!("{stage}/bundle");
    let flag = if force { " --force" } else { "" };
    let expected = format!(
        "sh '{bundle}/scripts/echo-install.sh' --device install --bundle '{bundle}' --release-version '0.2.0' --config '{remote}'{flag}"
    );
    let dispatches: Vec<_> = ledger
        .iter()
        .enumerate()
        .filter(|(_, args)| args.last().is_some_and(|arg| arg.contains("--device install --bundle")))
        .collect();
    let [(index, dispatch)] = dispatches.as_slice() else {
        return Err("expected exactly one bundled install dispatch".into());
    };
    assert_eq!(dispatch.as_slice(), ["-s", "SYNTHETIC-DEVICE", "shell", &expected]);
    assert_eq!(ledger[index - 1].as_slice(), push.as_slice());
    Ok(())
}

#[test]
fn t3_verified_release_dispatches_force_and_resumes_plain_install() -> Result {
    let bytes = b"controlled archive boundary".to_vec();
    let sums = format!("{}  echo-0.2.0-armv7.tar.gz\r\n", digest(&bytes));
    let (base, handle) = server(sums, bytes, 4)?;
    for force in [true, false] {
        let fixture = Fixture::new()?;
        let config = fixture.0.join("config ü ' ; $.toml");
        fs::write(&config, b"synthetic private config")?;
        let mut command = fixture.install();
        command.args(["--release-base", &base, "--config"]).arg(&config);
        if force {
            command.arg("--force");
        } else {
            fs::write(fixture.0.join("link-state"), "linked")?;
            fs::write(fixture.0.join("process-user"), "nobody")?;
        }
        let output = command.output()?;
        let result = private_output(&output)?;
        assert_eq!(output.status.code(), Some(0), "{result}");
        assert_eq!(result["version"], "0.2.0");
        assert_eq!(result["setup"], "complete");
        let ledger = fixture.ledger()?;
        assert_eq!(ledger[0], ["devices"]);
        assert_config_dispatch(&ledger, &config, force)?;
        assert_guided_start(&ledger, force)?;
        assert_eq!(fs::read_to_string(fixture.0.join("process-user"))?, "nobody");
    }
    handle.join().map_err(|_| "server panicked")??;
    Ok(())
}

fn assert_guided_start(ledger: &[Vec<String>], links: bool) -> Result {
    let dispatch: Vec<_> = ledger
        .iter()
        .filter_map(|args| args.last())
        .filter(|arg| arg.contains("--device") && !arg.starts_with("sh -s -- --device setup-"))
        .collect();
    assert_eq!(dispatch[0], "sh -s -- --device stop");
    assert!(dispatch[1].contains("--device prepare-archive"));
    assert!(dispatch[3].ends_with("--device screen-setup"));
    let start = dispatch
        .iter()
        .position(|call| call.ends_with("--device start"))
        .ok_or("missing bundled start")?;
    assert_eq!(dispatch[start - 1], "sh -s -- --device ensure-signin");
    assert_eq!(dispatch[start + 1], "sh -s -- --device status");
    assert_eq!(
        dispatch
            .iter()
            .filter(|call| call.as_str() == "sh -s -- --device link")
            .count(),
        usize::from(links)
    );
    assert!(
        dispatch
            .last()
            .is_some_and(|call| call.starts_with("sh -s -- --device confirm-check '"))
    );
    assert_started_then_observed_then_cleaned(ledger);
    Ok(())
}

#[tokio::test]
async fn t3_invalid_force_exits_before_contacting_mirror_or_adb() -> Result {
    let fixture = Fixture::new()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let missing = fixture.0.join("SYNTHETIC-PRIVATE-PATH.toml");
    for (args, code) in [
        (vec!["install", "--force"], 2),
        (vec!["install", "--force", "--config", "candidate", "--force"], 2),
        (vec!["update", "--force", "--config", "candidate"], 2),
        (vec!["status", "--force"], 2),
        (
            vec!["install", "--force", "--config", "candidate", "--from-dir", "bundle"],
            2,
        ),
        (
            vec![
                "install",
                "--force",
                "--config",
                missing.to_str().ok_or("fixture path")?,
            ],
            1,
        ),
        (
            vec![
                "install",
                "--force",
                "--config",
                fixture.0.to_str().ok_or("fixture path")?,
            ],
            1,
        ),
    ] {
        let mut command = tokio::process::Command::from(fixture.command());
        command.kill_on_drop(true).args(args).args([
            "--json",
            "--adb",
            env!("CARGO_BIN_EXE_echo-setup-test-adb"),
            "--release-base",
            &base,
        ]);
        let output = tokio::select! {
            biased;
            _ = listener.accept() => return Err("invalid force contacted the release mirror".into()),
            output = command.output() => output?,
        };
        assert_eq!(output.status.code(), Some(code));
        if code == 2 {
            assert!(output.stdout.is_empty());
            assert_eq!(output.stderr, b"echo-setup: invalid usage; see --help\n");
        } else {
            assert_eq!(private_output(&output)?["error"], "config_unreadable");
            assert!(output.stderr.is_empty());
        }
        assert_eq!(fixture.ledger()?, Vec::<Vec<String>>::new());
    }
    assert_eq!(
        listener.into_std()?.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    Ok(())
}
