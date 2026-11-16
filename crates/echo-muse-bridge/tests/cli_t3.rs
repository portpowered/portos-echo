//! Component T3: real adapter processes with a Cargo-prebuilt Rust fake, no network.
#![cfg(test)]
use std::future::{Future, pending};

use echo_agent_api::{AgentBackend, Task};
use echo_contracts::task::TaskEvent;
use echo_muse_bridge::{CliGadget, Clock, Gadget, GadgetError, InMemoryStore, MuseBackend, Reply, SideChat};
use serde_json::{Value, json};

struct Child {
    directory: tempfile::TempDir,
    gadget: CliGadget,
}

impl Child {
    fn new(stdout: &str, stderr: &str, exit: u8) -> Self {
        let directory = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let program = directory.path().join(if cfg!(windows) { "fake.exe" } else { "fake" });
        // Link the prebuilt artifact without a writable executable handle during parallel spawns.
        std::fs::hard_link(env!("CARGO_BIN_EXE_echo-muse-fake-gadget"), &program).unwrap();
        std::fs::write(
            directory.path().join("script.json"),
            json!({"stdout": stdout, "stderr": stderr, "exit": exit}).to_string(),
        )
        .unwrap();
        Self {
            directory,
            gadget: CliGadget { program },
        }
    }

    fn capture(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.directory.path().join("capture.json")).unwrap()).unwrap()
    }
}

#[tokio::test]
async fn t3_exact_argv_stdin_and_wait_endpoints_preserve_text_without_shell() {
    for wait in [1, 600] {
        let child = Child::new("answer\n", "", 0);
        let session = SideChat::fresh().unwrap();
        let text = "spaces 水 \"quotes\" ; $(touch unwanted) & | > unwanted";
        assert_eq!(
            child.gadget.send_user_msg(session, text, wait).await.unwrap(),
            Reply {
                text: "answer\n".into(),
                complete: true
            }
        );
        assert_eq!(
            child.capture(),
            json!({"argv": ["send-user-msg", "--session-id", session.to_string(), "--wait", wait.to_string(), text], "stdin": ""})
        );
        assert!(!child.directory.path().join("unwanted").exists());
    }
}

#[tokio::test]
async fn t3_exit_three_preserves_partial_output_and_trust_and_other_errors_are_exact() {
    let partial = Child::new("Still working\n", "", 3);
    let session = SideChat::fresh().unwrap();
    assert_eq!(
        partial.gadget.send_user_msg(session, "test", 1).await.unwrap(),
        Reply {
            text: "Still working\n".into(),
            complete: false
        }
    );
    let trust = Child::new("", "private diagnostic", 78);
    assert!(matches!(
        trust.gadget.send_user_msg(session, "test", 1).await,
        Err(GadgetError::TrustStore)
    ));
    for (stderr, expected) in [("a\nboom\n", "boom"), ("", "")] {
        let failure = Child::new("", stderr, 9);
        let error = failure.gadget.send_user_msg(session, "test", 1).await.unwrap_err();
        assert!(matches!(error, GadgetError::Exit { code: 9, reason } if reason == expected));
    }
}

#[tokio::test]
async fn t3_invalid_wait_never_spawns_and_missing_program_is_io_error() {
    let child = Child::new("", "", 0);
    for wait in [0, 601] {
        assert!(matches!(
            child
                .gadget
                .send_user_msg(SideChat::fresh().unwrap(), "test", wait)
                .await,
            Err(GadgetError::Wait)
        ));
        assert!(!child.directory.path().join("capture.json").exists());
    }
    let missing = CliGadget {
        program: child.directory.path().join("absent"),
    };
    assert!(matches!(
        missing.send_user_msg(SideChat::fresh().unwrap(), "test", 1).await,
        Err(GadgetError::Io(_))
    ));
}

struct FrozenClock;

impl Clock for FrozenClock {
    fn now(&self) -> u64 {
        100
    }
    fn wait_until(&self, _: u64) -> impl Future<Output = ()> + Send {
        pending()
    }
}

#[tokio::test]
async fn t3_assembled_backend_process_reply_report_terminal_flow() {
    let child = Child::new("Done! The job completed successfully.", "", 0);
    let backend = MuseBackend::new(
        CliGadget {
            program: child.gadget.program.clone(),
        },
        FrozenClock,
        InMemoryStore::default(),
    );
    let task: Task = serde_json::from_value(json!({"delegation_id": "delegation-test", "instructions": "Read light", "context": {"target": "test-light"}, "deadline_seconds": 60, "max_tool_calls": 8})).unwrap();
    let id = backend.submit(task).await.unwrap();
    let capture = child.capture();
    let argv = capture["argv"].as_array().unwrap();
    assert_eq!(argv.len(), 6);
    let session: SideChat = serde_json::from_value(argv[2].clone()).unwrap();
    let message = format!(
        "Read light\nContext: {{\"target\":\"test-light\"}}\nTask ID: {id}\nWhen you are finished, call the echo.report.result command with task_id {id}, status completed or failed, and a short summary. Do not report success any other way."
    );
    assert_eq!(
        capture,
        json!({"argv": ["send-user-msg", "--session-id", session.to_string(), "--wait", "1", message], "stdin": ""})
    );
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Progress {
            task_id: id.clone(),
            message: "Done! The job completed successfully.".into()
        })
    );
    let accepted = backend
        .report_intake()
        .accept(&json!({"task_id": id, "status": "completed", "summary": "light is on"}).to_string())
        .unwrap();
    assert_eq!(accepted, json!({"accepted": true, "task_id": id}));
    assert_eq!(
        backend.next_event(&id).await,
        Some(TaskEvent::Completed {
            task_id: id.clone(),
            summary: "light is on".into()
        })
    );
    assert_eq!(backend.next_event(&id).await, None);
}

#[test]
fn side_chat_requires_canonical_uuid_on_restore() {
    for invalid in [
        "not-uuid",
        "11111111111141118111111111111111",
        "AAAAAAAA-1111-4111-8111-111111111111",
    ] {
        assert!(serde_json::from_value::<SideChat>(json!(invalid)).is_err());
    }
}
