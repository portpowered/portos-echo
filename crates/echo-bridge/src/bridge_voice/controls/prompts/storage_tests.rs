//! Network-free prompt storage witnesses, isolated homes and injected clock/IO.
use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    time::{Duration, UNIX_EPOCH},
};

fn history(home: &Path) -> Vec<(String, String)> {
    let path = home.join("prompts/.history");
    if !path.exists() {
        return vec![];
    }
    let mut entries: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                fs::read_to_string(entry.path()).unwrap(),
            )
        })
        .collect();
    entries.sort_by_key(|(name, _)| {
        name.split_once('-')
            .unwrap()
            .1
            .trim_end_matches(".md")
            .parse::<u128>()
            .unwrap()
    });
    entries
}

#[test]
fn agent_tools_prompt_defaults_validation_exact_utf8_and_unchanged_refusals() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    assert_eq!(
        super::super::read_base(&home, "voice"),
        Ok(echo_config::LIVE_INSTRUCTIONS.into())
    );
    assert_eq!(
        super::super::read_base(&home, "runner"),
        Ok(echo_config::RUNNER_INSTRUCTIONS.into())
    );
    assert!(!home.exists());
    assert_eq!(super::super::write_base(&home, "other", "x"), Err("invalid_arguments"));
    for text in [String::new(), " \n\t".into(), "x".repeat(32_769)] {
        assert_eq!(write(&home, Kind::Voice, &text), Err("invalid_prompt"));
    }
    assert!(!home.exists());
    for text in ["a".into(), "語".repeat(10_922) + "ab"] {
        write(&home, Kind::Voice, &text).unwrap();
        assert_eq!(read(&home, Kind::Voice), Ok(Some(text.clone())));
        refused_replacements(&home, &text);
    }
    assert_eq!(
        history(&home).iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>(),
        ["a"]
    );
    assert_eq!(read(&home, Kind::Runner), Ok(None));
}

fn refused_replacements(home: &Path, text: &str) {
    let before = history(home);
    for bad in [String::new(), " \n\t".into(), "語".repeat(10_923)] {
        assert_eq!(write(home, Kind::Voice, &bad), Err("invalid_prompt"));
        assert_eq!(history(home), before);
        assert_eq!(read(home, Kind::Voice), Ok(Some(text.into())));
    }
}

#[test]
fn agent_tools_prompt_shared_newest_twenty_equal_clock_restart_and_modes() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let now = UNIX_EPOCH + Duration::from_secs(1);
    for kind in [Kind::Voice, Kind::Runner] {
        commit(home, kind, "initial", now, Fault::None).unwrap();
    }
    for number in 0..23 {
        let kind = if number % 2 == 0 { Kind::Voice } else { Kind::Runner };
        commit(home, kind, &format!("version-{number}"), now, Fault::None).unwrap();
    }
    let entries = history(home);
    assert_eq!(entries.len(), 20);
    let expected: Vec<_> = (1..21).map(|number| format!("version-{number}")).collect();
    assert_eq!(
        entries.iter().map(|(_, text)| text).collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );
    assert_eq!(entries.first().unwrap().0, "runner-1000000003.md");
    assert_eq!(entries.last().unwrap().0, "voice-1000000022.md");
    assert_eq!(read(home, Kind::Voice), Ok(Some("version-22".into())));
    assert_eq!(read(home, Kind::Runner), Ok(Some("version-21".into())));
    private_modes(home);
    // A reopened writer with an older clock must still choose a unique name.
    commit(home, Kind::Runner, "reopened", UNIX_EPOCH, Fault::None).unwrap();
    assert_eq!(
        history(home).last().unwrap(),
        &("runner-1000000023.md".into(), "version-21".into())
    );
}

fn private_modes(home: &Path) {
    for dir in [home.join("prompts"), home.join("prompts/.history")] {
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                assert_eq!(entry.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            }
        }
    }
}

#[test]
fn agent_tools_prompt_links_nonregular_history_and_ancestors_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("private");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("voice.md"), "sentinel").unwrap();
    for target in ["prompts", "prompts/voice.md", "prompts/.history"] {
        let home = root.path().join(target.replace('/', "-"));
        fs::create_dir(&home).unwrap();
        if target != "prompts" {
            fs::create_dir(home.join("prompts")).unwrap();
        }
        let destination = if target.ends_with("voice.md") {
            outside.join("voice.md")
        } else {
            outside.clone()
        };
        symlink(destination, home.join(target)).unwrap();
        assert_eq!(write(&home, Kind::Voice, "replacement"), Err("symlink"));
        if target != "prompts/.history" {
            assert_eq!(read(&home, Kind::Voice), Err("symlink"));
        }
        assert_eq!(fs::read_to_string(outside.join("voice.md")).unwrap(), "sentinel");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    }
    let home = root.path().join("hardlink");
    fs::create_dir_all(home.join("prompts")).unwrap();
    fs::hard_link(outside.join("voice.md"), home.join("prompts/voice.md")).unwrap();
    assert_eq!(write(&home, Kind::Voice, "replacement"), Err("not_regular"));
    fs::remove_file(home.join("prompts/voice.md")).unwrap();
    fs::create_dir(home.join("prompts/voice.md")).unwrap();
    assert_eq!(read(&home, Kind::Voice), Err("not_regular"));
    let alias = root.path().join("alias");
    symlink(&outside, &alias).unwrap();
    assert_eq!(write(&alias.join("nested"), Kind::Voice, "replacement"), Err("symlink"));
    assert!(!outside.join("nested").exists());
}

#[test]
fn agent_tools_prompt_precommit_failures_keep_old_postcommit_failure_is_recoverable() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    write(home, Kind::Voice, "old").unwrap();
    assert_eq!(
        commit(home, Kind::Voice, "new", UNIX_EPOCH, Fault::Archive),
        Err("history")
    );
    assert_eq!(read(home, Kind::Voice), Ok(Some("old".into())));
    assert_eq!(history(home), []);
    assert_eq!(commit(home, Kind::Voice, "new", UNIX_EPOCH, Fault::Publish), Err("io"));
    assert_eq!(read(home, Kind::Voice), Ok(Some("old".into())));
    assert_eq!(history(home), [("voice-1.md".into(), "old".into())]);
    assert_eq!(
        commit(home, Kind::Voice, "committed", UNIX_EPOCH, Fault::Prune),
        Err("history")
    );
    assert_eq!(read(home, Kind::Voice), Ok(Some("committed".into())));
    assert_eq!(history(home).len(), 2);
    write(home, Kind::Runner, "reopened").unwrap();
    let names: Vec<_> = fs::read_dir(home.join("prompts"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names.len(), 3);
    assert!(
        names
            .iter()
            .all(|name| ["voice.md", "runner.md", ".history"].contains(&name.as_str()))
    );
    assert!(
        fs::read_dir(home.join("prompts/.history")).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_str()
            .unwrap()
            .starts_with(".note-tmp"))
    );
}

#[test]
fn agent_tools_prompt_concurrent_callers_keep_serial_versions_and_archives() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), Kind::Voice, "initial").unwrap();
    std::thread::scope(|scope| {
        let writers: Vec<_> = (0..8)
            .map(|index| {
                let home = root.path();
                scope.spawn(move || super::super::write_base(home, "voice", &format!("writer-{index}")).unwrap())
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
    });
    let mut texts: Vec<_> = history(root.path()).into_iter().map(|(_, text)| text).collect();
    texts.push(read(root.path(), Kind::Voice).unwrap().unwrap());
    texts.sort();
    let mut expected = vec!["initial".to_string()];
    expected.extend((0..8).map(|index| format!("writer-{index}")));
    assert_eq!(texts, expected);
}

#[test]
fn agent_tools_prompt_repeated_publish_failures_keep_history_bounded() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), Kind::Voice, "old").unwrap();
    for _ in 0..25 {
        assert_eq!(
            commit(root.path(), Kind::Voice, "new", UNIX_EPOCH, Fault::Publish),
            Err("io")
        );
        assert_eq!(read(root.path(), Kind::Voice), Ok(Some("old".into())));
    }
    let entries = history(root.path());
    assert_eq!(entries.len(), 20);
    assert_eq!(entries.first().unwrap().0, "voice-6.md");
    assert_eq!(entries.last().unwrap().0, "voice-25.md");
    assert!(entries.iter().all(|(_, text)| text == "old"));
}
