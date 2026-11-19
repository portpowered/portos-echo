//! T1: a home below ancestors the bridge user may search but not list, as on
//! Android where `/data` and `/data/local` grant a non-root user only `x`.
use super::*;
use crate::bridge_voice::prompts;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Restores the mode on drop so the temporary tree can be removed.
struct Gate(PathBuf);

impl Gate {
    /// `None` when this user ignores directory modes (root): the walk cannot be
    /// distinguished there, so the caller skips instead of claiming a pass.
    fn close(path: &Path) -> Option<Self> {
        fs::set_permissions(path, fs::Permissions::from_mode(0o111)).unwrap();
        let gate = Self(path.into());
        fs::read_dir(path).is_err().then_some(gate)
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

fn call(notes: &Notes, name: &str, args: &Value) -> ToolResult {
    notes.call(name, args, "1:t1:u1:c").unwrap()
}

#[test]
fn agent_tools_notes_and_prompts_work_below_search_only_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let outer = root.path().join("outer");
    let home = outer.join("inner/home");
    fs::create_dir_all(&home).unwrap();
    let Some(_inner) = Gate::close(&outer.join("inner")) else {
        eprintln!("skipped: this user bypasses directory permissions");
        return;
    };
    let _outer = Gate::close(&outer).unwrap();
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
    assert_eq!(call(&notes, "notes_list", &json!({})).content, json!({"files":[]}));
    let written = call(&notes, "notes_write", &json!({"path":"a/b.md","content":"body"}));
    assert_eq!(written.content, json!({"path":"a/b.md","bytes":4}));
    let read = call(&notes, "notes_read", &json!({"path":"a/b.md"}));
    assert_eq!(read.content, json!({"path":"a/b.md","bytes":4,"content":"body"}));
    let listed = call(&notes, "notes_list", &json!({"directory":"a"}));
    assert_eq!(listed.content["files"].as_array().unwrap().len(), 1);
    assert_eq!(listed.content["files"][0]["path"], "a/b.md");
    assert_eq!(listed.content["files"][0]["bytes"], 4);
    for content in ["first", "second"] {
        let result = call(&notes, "prompt_write", &json!({"which":"voice","content":content}));
        assert_eq!(result.content["bytes"], content.len());
        assert!(!result.is_error);
    }
    let read = call(&notes, "prompt_read", &json!({"which":"voice"}));
    assert_eq!(read.content, json!({"which":"voice","bytes":6,"content":"second"}));
    assert_eq!(prompts::read_base(&home, "voice"), Ok("second".into()));
    assert_eq!(fs::read_dir(home.join("prompts/.history")).unwrap().count(), 1);
    assert_eq!(fs::read(home.join("notes/a/b.md")).unwrap(), b"body");
    for path in ["notes", "notes/a", "prompts", "prompts/.history"] {
        assert_eq!(
            fs::metadata(home.join(path)).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
