//! T3 notes refusal and restart witnesses through the production RPC dispatcher.
use super::*;
use std::{fs, os::unix::fs::symlink};

fn home(root: &std::path::Path) -> HomeConfig {
    HomeConfig {
        dir: root.join("home"),
        agent_tools: AgentTools::default(),
    }
}

fn refused(code: &str, message: &str) -> Value {
    json!({"success":false,"contentItems":[{"type":"inputText","text":json!({"error":code,"message":message}).to_string()}]})
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_notes_invalid_paths_and_oversize_leave_home_absent() {
    let root = tempfile::tempdir().unwrap();
    let home = home(root.path());
    let trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    let mut steps = vec![];
    for path in ["../x.md", "/x.md", "x.txt", "a/b/c/d/x.md", "X.md"] {
        steps.push((
            "notes_write",
            json!({"path":path,"content":"private marker"}),
            refused("invalid_path", "Use a relative markdown path inside notes."),
        ));
    }
    steps.push((
        "notes_write",
        json!({"path":"x.md","content":"x".repeat(66_560)}),
        refused("note_too_large", "A note may contain at most 65536 bytes."),
    ));
    scripted(Some(&home), &trace, steps).await;
    assert!(!home.dir.exists());
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_notes_reopened_utf8_replacement_and_sorted_subtree() {
    let root = tempfile::tempdir().unwrap();
    let home = home(root.path());
    let trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    scripted(
        Some(&home),
        &trace,
        vec![
            ("notes_list", json!({}), result(&json!({"files":[]}))),
            (
                "notes_write",
                json!({"path":"house/z.md","content":"old"}),
                result(&json!({"path":"house/z.md","bytes":3})),
            ),
            (
                "notes_write",
                json!({"path":"house/z.md","content":"語"}),
                result(&json!({"path":"house/z.md","bytes":3})),
            ),
            (
                "notes_write",
                json!({"path":"house/a.md","content":""}),
                result(&json!({"path":"house/a.md","bytes":0})),
            ),
        ],
    )
    .await;
    for name in ["a", "z"] {
        fs::File::open(home.dir.join(format!("notes/house/{name}.md")))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
    }
    scripted(
        Some(&home),
        &trace,
        vec![
            (
                "notes_read",
                json!({"path":"house/z.md"}),
                result(&json!({"path":"house/z.md","bytes":3,"content":"語"})),
            ),
            (
                "notes_list",
                json!({"directory":"house"}),
                result(&json!({"files":[
                    {"path":"house/a.md","bytes":0,"modified_unix_ms":1000},
                    {"path":"house/z.md","bytes":3,"modified_unix_ms":1000}
                ]})),
            ),
        ],
    )
    .await;
    assert_eq!(fs::read_dir(home.dir.join("notes/house")).unwrap().count(), 2);
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_notes_links_and_fifo_refuse_without_private_effects() {
    let root = tempfile::tempdir().unwrap();
    let home = home(root.path());
    let trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    let private = root.path().join("private");
    fs::create_dir(&private).unwrap();
    fs::write(private.join("x.md"), "AGENT_NOTE_PRIVATE_MARKER").unwrap();
    fs::create_dir(&home.dir).unwrap();
    for location in ["root", "parent", "leaf", "hard", "fifo"] {
        let notes = home.dir.join("notes");
        let path = if location == "parent" { "linked/x.md" } else { "x.md" };
        if location == "root" {
            symlink(&private, &notes).unwrap();
        } else {
            fs::create_dir(&notes).unwrap();
            match location {
                "parent" => symlink(&private, notes.join("linked")).unwrap(),
                "leaf" => symlink(private.join("x.md"), notes.join("x.md")).unwrap(),
                "hard" => fs::hard_link(private.join("x.md"), notes.join("x.md")).unwrap(),
                _ => rustix::fs::mkfifoat(rustix::fs::CWD, notes.join("x.md"), rustix::fs::Mode::RUSR).unwrap(),
            }
        }
        let (code, message) = if matches!(location, "hard" | "fifo") {
            ("not_regular", "Expected a private regular file or directory.")
        } else {
            ("symlink", "Symbolic links are not permitted.")
        };
        scripted(
            Some(&home),
            &trace,
            vec![
                ("notes_read", json!({"path":path}), refused(code, message)),
                (
                    "notes_write",
                    json!({"path":path,"content":"overwrite"}),
                    refused(code, message),
                ),
                ("notes_list", json!({}), refused(code, message)),
            ],
        )
        .await;
        assert_eq!(
            fs::read_to_string(private.join("x.md")).unwrap(),
            "AGENT_NOTE_PRIVATE_MARKER"
        );
        assert_eq!(fs::read_dir(&private).unwrap().count(), 1);
        if location == "root" {
            fs::remove_file(notes).unwrap();
        } else {
            fs::remove_dir_all(notes).unwrap();
        }
    }
    assert_eq!(
        fs::read_to_string(root.path().join("trace"))
            .unwrap()
            .matches("AGENT_NOTE_PRIVATE_MARKER")
            .count(),
        0
    );
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_notes_total_limits_refuse_growth_and_allow_overwrite() {
    let root = tempfile::tempdir().unwrap();
    let home = home(root.path());
    let trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    let notes = home.dir.join("notes");
    fs::create_dir_all(&notes).unwrap();
    for index in 0..256 {
        fs::write(notes.join(format!("{index}.md")), vec![b'a'; 65_536]).unwrap();
    }
    scripted(
        Some(&home),
        &trace,
        vec![
            (
                "notes_write",
                json!({"path":"extra.md","content":"x"}),
                refused(
                    "notes_bytes_limit",
                    "Notes may contain at most 16777216 bytes in total.",
                ),
            ),
            (
                "notes_write",
                json!({"path":"0.md","content":"語"}),
                result(&json!({"path":"0.md","bytes":3})),
            ),
        ],
    )
    .await;
    assert!(!notes.join("extra.md").exists());
    for index in 0..1024 {
        fs::write(notes.join(format!("{index}.md")), "").unwrap();
    }
    scripted(
        Some(&home),
        &trace,
        vec![
            (
                "notes_write",
                json!({"path":"extra.md","content":""}),
                refused("notes_files_limit", "Notes may contain at most 1024 files."),
            ),
            (
                "notes_write",
                json!({"path":"0.md","content":"new"}),
                result(&json!({"path":"0.md","bytes":3})),
            ),
        ],
    )
    .await;
    assert_eq!(fs::read_dir(&notes).unwrap().count(), 1024);
    assert_eq!(fs::read_to_string(notes.join("0.md")).unwrap(), "new");
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_notes_absent_home_catalog_and_forged_calls_are_inert() {
    let root = tempfile::tempdir().unwrap();
    let trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    let steps = NAMES
        .into_iter()
        .map(|name| {
            (
                name,
                json!({"path":"x.md","content":"forged"}),
                json!({"success":false,"contentItems":[{"type":"inputText","text":"tool request refused"}]}),
            )
        })
        .collect();
    scripted(None, &trace, steps).await;
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_notes_trace_has_exact_receipts_and_zero_payload_markers() {
    let root = tempfile::tempdir().unwrap();
    let home = home(root.path());
    let trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    let marker = "AGENT_NOTE_PRIVATE_MARKER";
    scripted(
        Some(&home),
        &trace,
        vec![
            (
                "notes_write",
                json!({"path":"x.md","content":marker}),
                result(&json!({"path":"x.md","bytes":marker.len()})),
            ),
            (
                "notes_read",
                json!({"path":"x.md"}),
                result(&json!({"path":"x.md","bytes":marker.len(),"content":marker})),
            ),
            (
                "notes_read",
                json!({"path":marker}),
                refused("invalid_path", "Use a relative markdown path inside notes."),
            ),
        ],
    )
    .await;
    let text = fs::read_to_string(root.path().join("trace")).unwrap();
    assert_eq!(text.matches(marker).count(), 0);
    let rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|row: &Value| row["event"] == "agent.tool")
        .collect();
    assert_eq!(rows.len(), 3);
    for (index, name) in ["notes_write", "notes_read", "notes_read"].into_iter().enumerate() {
        let ok = index != 2;
        assert_eq!(
            rows[index],
            json!({"event":"agent.tool","elapsed_ms":rows[index]["elapsed_ms"],
            "provider_auth_mode":"api_key","tool":name,"path":if ok {Some("x.md")} else {None},
            "bytes":if ok {marker.len()} else {0},"result":if ok {"ok"} else {"invalid_path"}})
        );
    }
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_notes_stale_thread_and_completed_turn_cannot_write() {
    let root = tempfile::tempdir().unwrap();
    let home = home(root.path());
    let trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    let notes = Notes::new(Some(&home), trace.clone());
    let (mut io, mut server) = fixture();
    let client = drive(&mut io, &notes, &trace);
    let remote = async {
        opened(&mut server, "thread", &catalog(&notes)).await;
        let args = json!({"path":"x.md","content":"stale"});
        assert_eq!(
            tool(&mut server, "old-thread", "old", "notes_write", args.clone()).await,
            json!({"success":false,"contentItems":[]})
        );
        notification(
            &mut server,
            "turn/completed",
            json!({"threadId":"thread","turn":{"id":"turn"}}),
        )
        .await;
        assert_eq!(
            tool(&mut server, "thread", "late", "notes_write", args).await,
            json!({"success":false,"contentItems":[{"type":"inputText","text":"stale or duplicate tool request"}]})
        );
        closed(&mut server, "thread").await;
    };
    tokio::join!(client, remote);
    assert!(!home.dir.exists());
}

#[tokio::test(start_paused = true)]
async fn t3_agent_tools_notes_deadline_interrupts_matching_turn_and_refuses_late_write() {
    let root = tempfile::tempdir().unwrap();
    let home = home(root.path());
    let trace = Trace::open(&root.path().join("trace"), "api_key").unwrap();
    let notes = Notes::new(Some(&home), trace.clone());
    let (mut io, mut server) = fixture();
    let client = drive(&mut io, &notes, &trace);
    let remote = async {
        opened(&mut server, "thread", &catalog(&notes)).await;
        let request = read(&mut server).await;
        assert_eq!(request["method"], "turn/interrupt");
        assert_eq!(request["params"], json!({"threadId":"thread","turnId":"turn"}));
        write(&mut server, json!({"id":request["id"],"result":{}})).await;
        assert_eq!(
            tool(
                &mut server,
                "thread",
                "late",
                "notes_write",
                json!({"path":"x.md","content":"late"})
            )
            .await,
            json!({"success":false,"contentItems":[{"type":"inputText","text":"stale or duplicate tool request"}]})
        );
        closed(&mut server, "thread").await;
    };
    tokio::join!(client, remote);
    assert!(!home.dir.exists());
}
