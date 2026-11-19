//! T1: the single persistence owner, real temporary files, no runtime or sockets.
use super::{settings_atomic::Stage, *};
use std::{fs, path::Path};

const BASE: &str = "[echo]\nhost='127.0.0.1'\n[live]\ncredential_file='absent/live'\n[agent]\n[mcp]\n[screen]\npairing_token_file='absent/pairing'\n[audit]\nlog_dir='absent/logs'\n";

fn setup() -> (tempfile::TempDir, SettingsStore) {
    let root = tempfile::tempdir().unwrap();
    let mut base = BridgeConfig::from_toml_str(BASE).unwrap();
    base.home = Some(echo_config::HomeConfig {
        dir: root.path().join("home"),
    });
    fs::write(root.path().join("bridge.toml"), BASE).unwrap();
    let store = SettingsStore::open(base, SettingsOverlay::default()).unwrap();
    (root, store)
}

fn patch(text: &str) -> SettingsOverlay {
    SettingsOverlay::from_toml_str(text).unwrap()
}

fn files(path: &Path) -> Vec<String> {
    let mut files: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    files
}

#[test]
fn screen_settings_store_sparse_saves_preserve_siblings_base_and_effective_values() {
    let (root, mut store) = setup();
    assert!(store.save(&patch("[agent]\nmax_tool_calls=4")).unwrap());
    assert_eq!(
        fs::read_to_string(store.home.join("settings.toml")).unwrap(),
        "[agent]\nmax_tool_calls = 4\n"
    );
    assert!(store.save(&patch("[codex]\nvoice='marin'")).unwrap());
    let persisted = fs::read_to_string(store.home.join("settings.toml")).unwrap();
    assert_eq!(persisted, "[agent]\nmax_tool_calls = 4\n\n[codex]\nvoice = \"marin\"\n");
    let effective = patch(&persisted).apply_to(&store.base).unwrap();
    assert_eq!(effective, *store.effective());
    assert_eq!(effective.agent.max_tool_calls, 4);
    assert_eq!(effective.codex_voice(), Some("marin"));
    assert_eq!(effective.voice.session_listen_seconds, 15);
    assert_eq!(fs::read_to_string(root.path().join("bridge.toml")).unwrap(), BASE);
    assert_eq!(files(&store.home), ["settings.toml"]);
}

#[test]
fn screen_settings_store_invalid_candidate_does_not_write_or_change_accepted_state() {
    let (_root, mut store) = setup();
    store.save(&patch("[agent]\nmax_tool_calls=4")).unwrap();
    let path = store.home.join("settings.toml");
    let previous = fs::read(&path).unwrap();
    let config = store.effective().clone();
    for (input, expected) in [
        (
            "[agent]\nmax_tool_calls=33",
            "agent.max_tool_calls: must be within 1..=32",
        ),
        (
            "[codex]\nvoice='SYNTHETIC_VALUE'",
            "codex.voice: expected default or a pinned realtime voice",
        ),
    ] {
        assert_eq!(store.save(&patch(input)).unwrap_err().to_string(), expected);
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_eq!(store.effective(), &config);
        assert_eq!(files(&store.home), ["settings.toml"]);
    }
}

#[test]
fn screen_settings_store_noop_does_not_replace_or_request_restart() {
    let (_root, mut store) = setup();
    // Explicit server default and an absent voice have identical runtime meaning.
    assert!(!store.save(&patch("[codex]\nvoice='default'")).unwrap());
    assert!(!store.save(&patch("[agent]\nmax_tool_calls=8")).unwrap());
    let before = fs::read(store.home.join("settings.toml")).unwrap();
    let duplicate = patch("[agent]\nmax_tool_calls=8");
    let mut stages = Vec::new();
    assert!(
        !store
            .save_with(&duplicate, |stage| {
                stages.push(stage);
                Ok(())
            })
            .unwrap()
    );
    assert_eq!(stages, []);
    assert_eq!(fs::read(store.home.join("settings.toml")).unwrap(), before);
    assert!(store.save(&patch("[voice]\nsession_listen_enabled=false")).unwrap());
    assert!(!store.effective().voice.session_listen_enabled);
}

#[test]
fn screen_settings_store_pre_rename_failures_retain_file_mode_and_state_and_remove_temp() {
    let (_root, mut store) = setup();
    store.save(&patch("[agent]\nmax_tool_calls=4")).unwrap();
    let path = store.home.join("settings.toml");
    let previous = fs::read(&path).unwrap();
    for failed in [Stage::Create, Stage::Write, Stage::FileSync, Stage::Rename] {
        let error = store
            .save_with(&patch("[agent]\nmax_tool_calls=5"), |stage| {
                if stage == failed {
                    Err(std::io::Error::other("SYNTHETIC_VALUE"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "Could not save file; previous file retained");
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_eq!(store.effective().agent.max_tool_calls, 4);
        assert_eq!(files(&store.home), ["settings.toml"]);
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o600);
    }
    assert!(store.save(&patch("[agent]\nmax_tool_calls=5")).unwrap());
}

#[test]
fn screen_settings_store_directory_sync_failure_reports_uncertainty_without_false_rollback() {
    let (_root, mut store) = setup();
    store.save(&patch("[agent]\nmax_tool_calls=4")).unwrap();
    let error = store
        .save_with(&patch("[agent]\nmax_tool_calls=5"), |stage| {
            if stage == Stage::DirectorySync {
                Err(std::io::Error::other("synthetic"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "File replaced but durability is uncertain; reload before saving again"
    );
    assert_eq!(
        fs::read_to_string(store.home.join("settings.toml")).unwrap(),
        "[agent]\nmax_tool_calls = 5\n"
    );
    assert_eq!(store.effective().agent.max_tool_calls, 4);
    assert_eq!(files(&store.home), ["settings.toml"]);
    assert!(matches!(
        store.save(&patch("[agent]\nmax_tool_calls=6")),
        Err(SettingsError::Durability)
    ));
}

#[test]
fn screen_settings_store_prompt_boundaries_preserve_exact_utf8_and_fixed_destinations() {
    let (_root, mut store) = setup();
    for kind in [PromptKind::Voice, PromptKind::Runner] {
        let path = store.home.join("prompts").join(kind.filename());
        for text in [
            "x".into(),
            "\u{8a9e}\n<script>inert text</script>\n".into(),
            "a".repeat(32_768),
        ] {
            store.save_prompt(kind, &text).unwrap();
            assert_eq!(fs::read(&path).unwrap(), text.as_bytes());
        }
        let before = fs::read(&path).unwrap();
        for text in [
            String::new(),
            " \n\t".into(),
            "a".repeat(32_769),
            "\u{8a9e}".repeat(10_923),
        ] {
            let error = store.save_prompt(kind, &text).unwrap_err();
            let reason = if text.trim().is_empty() {
                "empty prompt"
            } else {
                "prompt exceeds 32768 bytes"
            };
            assert_eq!(error.to_string(), format!("prompts/{}: {reason}", kind.filename()));
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }
    assert_eq!(files(&store.home.join("prompts")), ["runner.md", "voice.md"]);
    assert_eq!(files(&store.home), ["prompts"]);
    assert_eq!(store.effective().agent.max_tool_calls, 8);
}

#[test]
fn screen_settings_store_prompt_atomic_failures_preserve_previous_bytes_until_rename() {
    let (_root, mut store) = setup();
    store.save_prompt(PromptKind::Voice, "previous").unwrap();
    let directory = store.home.join("prompts");
    for failed in [
        Stage::Create,
        Stage::Write,
        Stage::FileSync,
        Stage::Rename,
        Stage::DirectorySync,
    ] {
        let error = settings_atomic::replace(&directory, "voice.md", b"replacement", |stage| {
            if stage == failed {
                Err(std::io::Error::other("synthetic"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        let after_rename = failed == Stage::DirectorySync;
        assert_eq!(matches!(error, SettingsError::Durability), after_rename);
        assert_eq!(
            fs::read_to_string(directory.join("voice.md")).unwrap(),
            if after_rename { "replacement" } else { "previous" }
        );
        assert_eq!(files(&directory), ["voice.md"]);
    }
}

#[test]
fn screen_settings_store_real_rename_failure_cleans_temp_and_does_not_accept_save() {
    let (_root, mut store) = setup();
    let target = store.home.join("settings.toml");
    fs::create_dir(&target).unwrap();
    assert!(matches!(
        store.save(&patch("[agent]\nmax_tool_calls=4")),
        Err(SettingsError::Persistence)
    ));
    assert!(target.is_dir());
    assert_eq!(store.effective().agent.max_tool_calls, 8);
    assert_eq!(files(&store.home), ["settings.toml"]);
    store.save_prompt(PromptKind::Voice, "voice").unwrap();
    assert_eq!(
        fs::read_to_string(store.home.join("prompts/voice.md")).unwrap(),
        "voice"
    );
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[cfg(unix)]
#[test]
fn screen_settings_store_replacements_are_owner_only_and_do_not_follow_destination_symlinks() {
    use std::os::unix::{fs::PermissionsExt, fs::symlink};
    let (root, mut store) = setup();
    let other = root.path().join("untouched");
    fs::write(&other, "outside target").unwrap();
    let path = store.home.join("settings.toml");
    symlink(&other, &path).unwrap();
    assert!(store.save(&patch("[agent]\nmax_tool_calls=4")).unwrap());
    assert!(!fs::symlink_metadata(&path).unwrap().is_symlink());
    assert_eq!(fs::read_to_string(&other).unwrap(), "outside target");
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&store.home), 0o700);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.save(&patch("[agent]\nmax_tool_calls=5")).unwrap());
    assert_eq!(mode(&path), 0o600);
    for kind in [PromptKind::Voice, PromptKind::Runner] {
        store.save_prompt(kind, "prompt").unwrap();
        assert_eq!(mode(&store.home.join("prompts").join(kind.filename())), 0o600);
    }
    assert_eq!(mode(&store.home.join("prompts")), 0o700);
}

#[test]
fn screen_settings_store_shared_owner_serializes_sparse_concurrent_saves() {
    let (_root, store) = setup();
    let shared = std::sync::Mutex::new(store);
    std::thread::scope(|scope| {
        for input in ["[agent]\nmax_tool_calls=4", "[codex]\nvoice='marin'"] {
            let shared = &shared;
            scope.spawn(move || assert!(shared.lock().unwrap().save(&patch(input)).unwrap()));
        }
    });
    let store = shared.into_inner().unwrap();
    let persisted = fs::read_to_string(store.home.join("settings.toml")).unwrap();
    assert_eq!(patch(&persisted).apply_to(&store.base).unwrap(), *store.effective());
    assert_eq!(store.effective().agent.max_tool_calls, 4);
    assert_eq!(store.effective().codex_voice(), Some("marin"));
    assert_eq!(files(&store.home), ["settings.toml"]);
}

#[test]
fn screen_settings_store_requires_configured_home_and_validates_adopted_overlay() {
    let base = BridgeConfig::from_toml_str(BASE).unwrap();
    assert!(matches!(
        SettingsStore::open(base, SettingsOverlay::default()),
        Err(SettingsError::Unavailable)
    ));
    let (_root, store) = setup();
    let base = store.base;
    assert!(matches!(
        SettingsStore::open(base.clone(), patch("[agent]\nmax_tool_calls=33")),
        Err(SettingsError::Validation(_))
    ));
    let store = SettingsStore::open(base, patch("[agent]\nmax_tool_calls=4")).unwrap();
    assert_eq!(store.effective().agent.max_tool_calls, 4);
    assert_eq!(files(&store.home), [] as [&str; 0]);
}
