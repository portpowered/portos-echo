use super::*;
use serde_json::{Value, json};

fn rows(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn settings_prompt_recovery_retains_text_until_valid_call_reload() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("prompts");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("voice.md"), " \n").unwrap();
    std::fs::write(dir.join("runner.md"), vec![b'x'; 32_769]).unwrap();
    let path = root.path().join("trace");
    let mut trace = Some(Trace::open(&path, "chatgpt").unwrap());
    let retained = echo_screen_contract::settings_v1::PromptTextsV1 {
        voice: "accepted voice\n".into(),
        runner: "accepted runner\n".into(),
    };
    scope_retained(Some(root.path()), Some(retained.clone()), async {
        assert_eq!(accepted().unwrap(), retained);
        publish(trace.as_mut().unwrap()).unwrap();
        assert_eq!(
            snapshot("unused", "unused", &mut trace).unwrap(),
            (retained.runner.clone(), retained.voice.clone())
        );
        std::fs::write(dir.join("voice.md"), "next voice\n").unwrap();
        std::fs::write(dir.join("runner.md"), "next runner\n").unwrap();
        assert_eq!(accepted().unwrap(), retained);
        assert_eq!(
            snapshot("unused", "unused", &mut trace).unwrap(),
            ("next runner\n".into(), "next voice\n".into())
        );
        Ok(())
    })
    .await
    .unwrap();
    let refused = rows(&path);
    assert_eq!(refused.len(), 2);
    assert_eq!(refused[0]["event"], "prompt.reload_refused");
    assert_eq!(refused[0]["prompt"], "runner");
    assert_eq!(refused[0]["reason"], "oversized");
    assert_eq!(refused[1]["event"], "prompt.reload_refused");
    assert_eq!(refused[1]["prompt"], "voice");
    assert_eq!(refused[1]["reason"], "empty");
}

#[tokio::test(start_paused = true)]
async fn settings_prompt_snapshot_uses_admitted_cache_until_call_reload() {
    assert!(matches!(accepted(), Err(Error::Input)));
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("prompts");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("voice.md"), "admitted voice\n").unwrap();
    let mut trace = Some(Trace::open(&root.path().join("trace"), "api_key").unwrap());
    scope(Some(root.path()), async {
        let initial = accepted().unwrap();
        assert_eq!(initial.voice, "admitted voice\n");
        assert_eq!(initial.runner, RUNNER_INSTRUCTIONS);
        std::fs::write(dir.join("voice.md"), "later voice\n").unwrap();
        assert_eq!(accepted().unwrap(), initial);
        snapshot("unused", "unused", &mut trace).unwrap();
        assert_eq!(accepted().unwrap().voice, "later voice\n");
        std::fs::write(dir.join("voice.md"), " \n").unwrap();
        snapshot("unused", "unused", &mut trace).unwrap();
        assert_eq!(accepted().unwrap().voice, "later voice\n");
        Ok(())
    })
    .await
    .unwrap();
    scope(None, async {
        assert!(matches!(accepted(), Err(Error::Input)));
        Ok(())
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn agent_home_missing_defaults_once_per_kind_per_runtime_and_no_home_is_silent() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("trace");
    let mut trace = Some(Trace::open(&path, "api_key").unwrap());
    for home in [None, Some(root.path()), Some(root.path())] {
        scope(home, async {
            publish(trace.as_mut().unwrap()).unwrap();
            for _ in 0..3 {
                assert_eq!(
                    snapshot("runner", "voice", &mut trace).unwrap(),
                    if home.is_some() {
                        (RUNNER_INSTRUCTIONS.into(), LIVE_INSTRUCTIONS.into())
                    } else {
                        ("runner".into(), "voice".into())
                    }
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    }
    let expected: Vec<_> = ["runner", "voice", "runner", "voice"]
        .into_iter()
        .map(|kind| json!({"event":"prompt.defaulted", "elapsed_ms":0, "provider_auth_mode":"api_key", "prompt":kind}))
        .collect();
    assert_eq!(rows(&path), expected);
}

#[tokio::test(start_paused = true)]
async fn agent_home_reload_refuses_bad_edits_and_missing_resets_only_its_kind() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("prompts");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("runner.md"), "runner\n\u{8a9e} \n").unwrap();
    std::fs::write(dir.join("voice.md"), "voice").unwrap();
    let path = root.path().join("trace");
    let mut trace = Some(Trace::open(&path, "api_key").unwrap());
    scope(Some(root.path()), async {
        for (bytes, reason) in [
            (vec![], "empty"),
            (b" \n\t".to_vec(), "empty"),
            (vec![b'a'; 32_769], "oversized"),
            (vec![255], "invalid_utf8"),
        ] {
            std::fs::write(dir.join("runner.md"), bytes).unwrap();
            assert_eq!(
                snapshot("unused", "unused", &mut trace).unwrap(),
                ("runner\n\u{8a9e} \n".into(), "voice".into())
            );
            assert_eq!(
                rows(&path).last().unwrap(),
                &json!({"event":"prompt.reload_refused", "elapsed_ms":0,
                "provider_auth_mode":"api_key", "prompt":"runner", "reason":reason})
            );
        }
        std::fs::remove_file(dir.join("runner.md")).unwrap();
        for _ in 0..2 {
            assert_eq!(
                snapshot("unused", "unused", &mut trace).unwrap(),
                (RUNNER_INSTRUCTIONS.into(), "voice".into())
            );
        }
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(rows(&path).len(), 5);
}

#[test]
fn agent_home_startup_rejects_bad_files_and_accepts_exact_byte_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("prompts");
    std::fs::create_dir(&dir).unwrap();
    for kind in ["voice", "runner"] {
        let file = dir.join(format!("{kind}.md"));
        for (bytes, reason) in [
            (vec![], "empty"),
            (b" \n\t".to_vec(), "empty"),
            (vec![b'a'; 32_769], "oversized"),
            (vec![255], "invalid_utf8"),
        ] {
            std::fs::write(&file, bytes).unwrap();
            assert_eq!(Prompts::open(root.path()).err(), Some(startup_error(kind, reason)));
        }
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert_eq!(
            Prompts::open(root.path()).err(),
            Some(startup_error(kind, "not_regular"))
        );
        std::fs::remove_dir(&file).unwrap();
        for text in ["a".into(), "\u{8a9e}".repeat(10_922) + "ab"] {
            std::fs::write(&file, &text).unwrap();
            assert_eq!(read(&file), Ok(Some(text)));
        }
        std::fs::remove_file(file).unwrap();
    }
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn agent_home_private_directories_existing_modes_and_reserved_settings() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("new/home");
    let prompts = Prompts::open(&home).unwrap();
    for path in [&home, &prompts.dir] {
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o750)).unwrap();
    }
    symlink("settings.toml", home.join("settings.toml")).unwrap();
    let _ = Prompts::open(&home).unwrap();
    for path in [&home, &prompts.dir] {
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o750);
    }
    let file = prompts.dir.join("voice.md");
    symlink("voice.md", &file).unwrap();
    assert_eq!(Prompts::open(&home).err(), Some(startup_error("voice", "unreadable")));
    let path = root.path().join("trace");
    let mut trace = Trace::open(&path, "api_key").unwrap();
    let mut prior = Prompt {
        kind: "voice",
        text: "previous".into(),
        missing: false,
        reported: false,
    };
    prior.reload(&prompts.dir, &mut trace).unwrap();
    assert_eq!(prior.text, "previous");
    assert_eq!(rows(&path)[0]["reason"], "unreadable");
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    prior.reload(&prompts.dir, &mut trace).unwrap();
    assert_eq!(prior.text, "previous");
    assert_eq!(rows(&path)[1]["reason"], "not_regular");
    let blocked = root.path().join("file");
    std::fs::write(&blocked, "file").unwrap();
    assert_eq!(
        Prompts::open(&blocked).err(),
        Some(Error::Prompt("home.dir: directory unavailable"))
    );
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn agent_home_trace_failure_cannot_claim_default_or_reload() {
    let root = tempfile::tempdir().unwrap();
    let mut trace = Some(Trace::open(Path::new("/dev/full"), "api_key").unwrap());
    scope(Some(root.path()), async {
        assert_eq!(publish(trace.as_mut().unwrap()), Err(Error::Trace));
        assert_eq!(snapshot("unused", "unused", &mut trace), Err(Error::Trace));
        std::fs::write(root.path().join("prompts/runner.md"), " ").unwrap();
        assert_eq!(snapshot("unused", "unused", &mut trace), Err(Error::Trace));
        Ok(())
    })
    .await
    .unwrap();
}

/// T1: the Android layout, where a non-root bridge may search but not list the
/// home's ancestors. Startup must open the home instead of refusing it. Root
/// ignores directory modes: there this passes for any walk and proves nothing.
#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn agent_home_opens_below_a_search_only_ancestor() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let gate = root.path().join("gate");
    let home = gate.join("home");
    std::fs::create_dir_all(home.join("prompts")).unwrap();
    std::fs::write(home.join("prompts/voice.md"), "voice").unwrap();
    std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o111)).unwrap();
    let opened = Prompts::open(&home).map(|prompts| (prompts.runner.text, prompts.voice.text));
    std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(opened, Ok((RUNNER_INSTRUCTIONS.into(), "voice".into())));
}
