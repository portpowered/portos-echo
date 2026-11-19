//! T1: a home below ancestors the bridge user may search but not list, as on
//! Android where `/data` and `/data/local` grant a non-root user only `x`.
//! Root ignores directory modes: there these pass for any walk and prove nothing.
use super::*;
use crate::bridge_voice::prompts;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Search-only while alive; restores the mode so the temporary tree can go.
struct Gate(PathBuf);

impl Gate {
    fn close(path: &Path) -> Self {
        fs::set_permissions(path, fs::Permissions::from_mode(0o111)).unwrap();
        Self(path.into())
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

fn call(notes: &Notes, name: &str, args: &Value) -> Value {
    let result = notes.call(name, args, "1:t1:u1:c").unwrap();
    assert!(!result.is_error, "{name}");
    result.content
}

fn notes_round_trip(notes: &Notes) {
    assert_eq!(call(notes, "notes_list", &json!({})), json!({"files":[]}));
    assert_eq!(
        call(notes, "notes_write", &json!({"path":"a/b.md","content":"body"})),
        json!({"path":"a/b.md","bytes":4})
    );
    assert_eq!(
        call(notes, "notes_read", &json!({"path":"a/b.md"})),
        json!({"path":"a/b.md","bytes":4,"content":"body"})
    );
    let listed = call(notes, "notes_list", &json!({"directory":"a"}));
    assert_eq!(listed["files"].as_array().unwrap().len(), 1);
    assert_eq!(listed["files"][0]["path"], "a/b.md");
    assert_eq!(listed["files"][0]["bytes"], 4);
}

fn prompts_round_trip(notes: &Notes, home: &Path) {
    for content in ["first", "second"] {
        assert_eq!(
            call(notes, "prompt_write", &json!({"which":"voice","content":content})),
            json!({"which":"voice","bytes":content.len(),"applies":"next_conversation"})
        );
    }
    assert_eq!(
        call(notes, "prompt_read", &json!({"which":"voice"})),
        json!({"which":"voice","bytes":6,"content":"second"})
    );
    assert_eq!(prompts::read_base(home, "voice"), Ok("second".into()));
    assert_eq!(fs::read_dir(home.join("prompts/.history")).unwrap().count(), 1);
}

#[test]
fn agent_tools_notes_and_prompts_work_below_search_only_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let outer = root.path().join("outer");
    let home = outer.join("inner/home");
    fs::create_dir_all(&home).unwrap();
    let _inner = Gate::close(&outer.join("inner"));
    let _outer = Gate::close(&outer);
    let notes = Notes::new(
        Some(&echo_config::HomeConfig {
            dir: home.clone(),
            agent_tools: echo_config::AgentTools {
                notes: true,
                prompts: true,
            },
        }),
        Trace::open(&root.path().join("trace"), "api_key").unwrap(),
    );
    notes_round_trip(&notes);
    prompts_round_trip(&notes, &home);
    assert_eq!(fs::read(home.join("notes/a/b.md")).unwrap(), b"body");
    for path in ["notes", "notes/a", "prompts", "prompts/.history"] {
        let mode = fs::metadata(home.join(path)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{path}");
    }
}
