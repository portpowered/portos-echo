//! T1/T2: network-free component filesystem and exact tool-envelope contracts.
use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

fn provider(root: &Path, enabled: bool) -> Notes {
    Notes::new(
        Some(&echo_config::HomeConfig {
            dir: root.join("home"),
            agent_tools: echo_config::AgentTools {
                notes: enabled,
                prompts: false,
            },
        }),
        Trace::open(&root.join("trace"), "api_key").unwrap(),
    )
}

fn call(notes: &Notes, name: &str, args: Value) -> ToolResult {
    notes.call(name, &args, "1:t1:u1:c").unwrap()
}

fn refusal(notes: &Notes, name: &str, args: Value, code: &str) {
    assert_eq!(
        call(notes, name, args),
        ToolResult {
            is_error: true,
            content: json!({"error":code,"message":message(code)}),
        }
    );
}

#[test]
fn agent_tools_notes_paths_and_schema_refuse_without_creating_home() {
    let root = tempfile::tempdir().unwrap();
    let notes = provider(root.path(), true);
    for path in [
        "../x.md",
        "/x.md",
        "a.txt",
        "a/b/c/d/x.md",
        "A.md",
        "a//x.md",
        "./x.md",
        "a/../x.md",
        "",
    ] {
        for name in ["notes_write", "notes_read"] {
            let mut args = json!({"path":path});
            if name == "notes_write" {
                args["content"] = json!("unchanged");
            }
            refusal(&notes, name, args, "invalid_path");
        }
    }
    for args in [
        json!({"path":"a.md"}),
        json!({"path":1,"content":"x"}),
        json!({"path":"a.md","content":1}),
        json!({"path":"a.md","content":"x","extra":true}),
        json!([]),
    ] {
        refusal(&notes, "notes_write", args, "invalid_arguments");
    }
    refusal(&notes, "notes_list", json!({"directory":"a/b/c/d"}), "invalid_path");
    refusal(&notes, "notes_list", json!({"directory":null}), "invalid_arguments");
    refusal(&notes, "notes_read", json!({"path":"missing.md"}), "not_found");
    assert_eq!(call(&notes, "notes_list", json!({})).content, json!({"files":[]}));
    assert!(!root.path().join("home").exists());
}

#[test]
fn agent_tools_notes_persist_utf8_replace_list_metadata_and_modes() {
    let root = tempfile::tempdir().unwrap();
    let notes = provider(root.path(), true);
    for (path, content) in [("z.md", ""), ("a/b/c/x.md", "remember"), ("a/first.md", "語")] {
        assert_eq!(
            call(&notes, "notes_write", json!({"path":path,"content":content})),
            ToolResult {
                is_error: false,
                content: json!({"path":path,"bytes":content.len()})
            }
        );
        let file = root.path().join("home/notes").join(path);
        assert_eq!(fs::read_to_string(&file).unwrap(), content);
        assert_eq!(fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600);
    }
    for directory in ["", "a", "a/b", "a/b/c"] {
        assert_eq!(
            fs::metadata(root.path().join("home/notes").join(directory))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let reopened = provider(root.path(), true);
    assert_eq!(
        call(&reopened, "notes_read", json!({"path":"a/b/c/x.md"})).content,
        json!({"path":"a/b/c/x.md","bytes":8,"content":"remember"})
    );
    let list = call(&reopened, "notes_list", json!({"directory":"a"})).content;
    let rows = list["files"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for (row, path, bytes) in [(rows[0].clone(), "a/b/c/x.md", 8), (rows[1].clone(), "a/first.md", 3)] {
        let modified = fs::metadata(root.path().join("home/notes").join(path))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        assert_eq!(row, json!({"path":path,"bytes":bytes,"modified_unix_ms":modified}));
    }
    assert!(!call(&notes, "notes_write", json!({"path":"a/first.md","content":"new"})).is_error);
    assert_eq!(
        call(&reopened, "notes_read", json!({"path":"a/first.md"})).content["content"],
        "new"
    );
}

#[test]
fn agent_tools_notes_byte_boundaries_are_utf8_bytes() {
    let root = tempfile::tempdir().unwrap();
    let notes = provider(root.path(), true);
    for content in [String::new(), "a".repeat(65_536), "語".repeat(21_845)] {
        assert_eq!(
            call(&notes, "notes_write", json!({"path":"bound.md","content":content})).content["bytes"],
            content.len()
        );
    }
    let previous = fs::read(root.path().join("home/notes/bound.md")).unwrap();
    for content in ["x".repeat(65_537), "x".repeat(66_560), "語".repeat(21_846)] {
        refusal(
            &notes,
            "notes_write",
            json!({"path":"bound.md","content":content}),
            "note_too_large",
        );
        assert_eq!(fs::read(root.path().join("home/notes/bound.md")).unwrap(), previous);
    }
}

#[test]
fn agent_tools_notes_exact_total_bytes_and_overwrite_accounting() {
    let root = tempfile::tempdir().unwrap();
    let notes = provider(root.path(), true);
    let dir = root.path().join("home/notes");
    fs::create_dir_all(&dir).unwrap();
    for index in 0..256 {
        fs::write(dir.join(format!("{index}.md")), vec![b'a'; 65_536]).unwrap();
    }
    let same = "b".repeat(65_536);
    assert_eq!(
        call(&notes, "notes_write", json!({"path":"0.md","content":same})).content["bytes"],
        65_536
    );
    refusal(
        &notes,
        "notes_write",
        json!({"path":"overflow.md","content":"x"}),
        "notes_bytes_limit",
    );
    assert!(!dir.join("overflow.md").exists());
    assert!(!call(&notes, "notes_write", json!({"path":"0.md","content":""})).is_error);
    assert!(!call(&notes, "notes_write", json!({"path":"overflow.md","content":same})).is_error);
    assert_eq!(
        call(&notes, "notes_list", json!({})).content["files"]
            .as_array()
            .unwrap()
            .len(),
        257
    );
}

#[test]
fn agent_tools_notes_exact_file_count_and_competing_writers() {
    let root = tempfile::tempdir().unwrap();
    let notes = provider(root.path(), true);
    let dir = root.path().join("home/notes");
    fs::create_dir_all(&dir).unwrap();
    for index in 0..1023 {
        fs::write(dir.join(format!("{index}.md")), "").unwrap();
    }
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            call(&notes, "notes_write", json!({"path":"a.md","content":"a"}))
        });
        let b = scope.spawn(|| {
            barrier.wait();
            call(&notes, "notes_write", json!({"path":"b.md","content":"b"}))
        });
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|result| !result.is_error).count(), 1);
    assert_eq!(
        results.iter().find(|result| result.is_error).unwrap().content["error"],
        "notes_files_limit"
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1024);
    assert!(!call(&notes, "notes_write", json!({"path":"0.md","content":"overwrite"})).is_error);
    refusal(
        &notes,
        "notes_write",
        json!({"path":"extra.md","content":""}),
        "notes_files_limit",
    );
}

#[test]
fn agent_tools_notes_disabled_and_no_home_have_no_filesystem_effects() {
    let root = tempfile::tempdir().unwrap();
    let notes = provider(root.path(), false);
    let absent = Notes::new(None, Trace::open(&root.path().join("trace"), "api_key").unwrap());
    for notes in [notes, absent] {
        assert_eq!(notes.specs(), vec![]);
        for name in NAMES {
            refusal(&notes, name, json!({"path":"x.md","content":"x"}), "disabled");
        }
    }
    assert!(!root.path().join("home").exists());
}

#[test]
fn agent_tools_notes_symlinks_hardlinks_and_nonregular_entries_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let notes = provider(root.path(), true);
    let outside = root.path().join("private");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("x.md"), "private sentinel").unwrap();
    fs::create_dir(root.path().join("home")).unwrap();
    symlink(&outside, root.path().join("home/notes")).unwrap();
    refusal(&notes, "notes_list", json!({}), "symlink");
    refusal(
        &notes,
        "notes_write",
        json!({"path":"x.md","content":"replace"}),
        "symlink",
    );
    fs::remove_file(root.path().join("home/notes")).unwrap();
    fs::create_dir(root.path().join("home/notes")).unwrap();
    symlink(&outside, root.path().join("home/notes/parent")).unwrap();
    refusal(&notes, "notes_read", json!({"path":"parent/x.md"}), "symlink");
    refusal(&notes, "notes_list", json!({}), "symlink");
    fs::remove_file(root.path().join("home/notes/parent")).unwrap();
    symlink(outside.join("x.md"), root.path().join("home/notes/x.md")).unwrap();
    refusal(&notes, "notes_read", json!({"path":"x.md"}), "symlink");
    fs::remove_file(root.path().join("home/notes/x.md")).unwrap();
    fs::hard_link(outside.join("x.md"), root.path().join("home/notes/x.md")).unwrap();
    for name in ["notes_read", "notes_write"] {
        let args = if name == "notes_read" {
            json!({"path":"x.md"})
        } else {
            json!({"path":"x.md","content":"replace"})
        };
        refusal(&notes, name, args, "not_regular");
    }
    refusal(&notes, "notes_list", json!({}), "not_regular");
    fs::remove_file(root.path().join("home/notes/x.md")).unwrap();
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        root.path().join("home/notes/x.md"),
        rustix::fs::Mode::RUSR,
    )
    .unwrap();
    refusal(&notes, "notes_read", json!({"path":"x.md"}), "not_regular");
    assert_eq!(fs::read_to_string(outside.join("x.md")).unwrap(), "private sentinel");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
}

#[test]
fn agent_tools_notes_failed_staging_keeps_old_bytes_and_cleans_temp() {
    let root = tempfile::tempdir().unwrap();
    let notes = provider(root.path(), true);
    call(&notes, "notes_write", json!({"path":"x.md","content":"old"}));
    let directory = notes_files::root(&root.path().join("home"), false).unwrap().unwrap();
    assert_eq!(notes_files::replace(&directory, "x.md", b"new", true), Err("io"));
    assert_eq!(fs::read(root.path().join("home/notes/x.md")).unwrap(), b"old");
    assert_eq!(fs::read_dir(root.path().join("home/notes")).unwrap().count(), 1);
}

#[test]
fn agent_tools_notes_receipts_never_include_contents_or_rejected_arguments() {
    let root = tempfile::tempdir().unwrap();
    let notes = provider(root.path(), true);
    let marker = "AGENT_NOTE_PRIVATE_MARKER";
    assert!(!call(&notes, "notes_write", json!({"path":"x.md","content":marker})).is_error);
    assert_eq!(
        call(&notes, "notes_read", json!({"path":"x.md"})).content["content"],
        marker
    );
    call(&notes, "notes_list", json!({}));
    refusal(&notes, "notes_read", json!({"path":marker}), "invalid_path");
    let trace = fs::read_to_string(root.path().join("trace")).unwrap();
    assert_eq!(trace.matches(marker).count(), 0);
    let rows: Vec<Value> = trace
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|row: &Value| row["event"] == "agent.tool")
        .collect();
    assert_eq!(rows.len(), 4);
    for (index, (tool, path, bytes, result)) in [
        ("notes_write", Some("x.md"), marker.len(), "ok"),
        ("notes_read", Some("x.md"), marker.len(), "ok"),
        ("notes_list", None, marker.len(), "ok"),
        ("notes_read", None, 0, "invalid_path"),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            rows[index],
            json!({"event":"agent.tool","elapsed_ms":rows[index]["elapsed_ms"],"provider_auth_mode":"api_key","tool":tool,"path":path,"bytes":bytes,"result":result})
        );
    }
}
