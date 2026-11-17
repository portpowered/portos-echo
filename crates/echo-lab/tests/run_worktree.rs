#![cfg(test)]
// T3: prebuilt CLI and fake tools, real per-test Git repositories, no hardware or services.
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Session {
    root: tempfile::TempDir,
    repo: PathBuf,
    live: PathBuf,
    stale: PathBuf,
}

fn git_context(command: &mut Command, repo: &Path) {
    for key in [
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(key);
    }
    command.env("GIT_DIR", repo.join(".git")).env("GIT_WORK_TREE", repo);
}

impl Session {
    fn new(stale: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo space");
        fs::create_dir(&repo).unwrap();
        let session = Self {
            repo,
            live: root.path().join("z live tree"),
            stale: root.path().join("a deleted tree"),
            root,
        };
        session.git(&["init"]);
        session.git(&[
            "-c",
            "user.name=Synthetic",
            "-c",
            "user.email=synthetic@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ]);
        for path in [&session.stale, &session.live] {
            session.git(&["worktree", "add", "--detach", path.to_str().unwrap(), "HEAD"]);
        }
        if stale {
            fs::remove_dir_all(&session.stale).unwrap();
        }
        fs::write(session.path("fixture.json"), "{}").unwrap();
        fs::write(session.path("utterance.wav"), b"synthetic input fixture").unwrap();
        session.scenario("B0", &json!([{"say":"utterance.wav"}]));
        session
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn git(&self, args: &[&str]) -> Vec<u8> {
        let mut command = Command::new("git");
        git_context(&mut command, &self.repo);
        let output = command.current_dir(&self.repo).args(args).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        output.stdout
    }

    fn listing(&self) -> Vec<u8> {
        self.git(&["worktree", "list", "--porcelain", "-z"])
    }

    fn scenario(&self, milestone: &str, steps: &Value) {
        fs::write(
            self.path("scenario.yaml"),
            json!({"id":"controlled","milestone":milestone,"tiers":["T3"],
                "setup":{},"steps":steps,"budgets":{},"negative":[]})
            .to_string(),
        )
        .unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_echo-lab"));
        git_context(&mut command, &self.repo);
        command
            .current_dir(self.root.path())
            .env("ECHO_LAB_RUN_FIXTURE", self.path("fixture.json"))
            .env("ECHO_LAB_RUN_LEDGER", self.path("ledger.jsonl"))
            .arg("run")
            .arg("scenario.yaml");
        command
    }

    fn device(&self, out: &Path) -> Output {
        let fake = env!("CARGO_BIN_EXE_echo-lab-fake-run");
        self.command()
            .args([
                "--driver",
                "device",
                "--bench",
                "ring",
                "--ring",
                "fixture-ring",
                "--port",
                fake,
                "--adb",
                fake,
                "--stt",
                fake,
                "--python",
                fake,
                "--out",
            ])
            .arg(out)
            .output()
            .unwrap()
    }

    fn report(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("artifacts/report.json")).unwrap()).unwrap()
    }

    fn refusal(&self, target: &Path, message: &str) {
        let before = self.listing();
        let output = self.device(target);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("echo-lab: run output: {message}\n")
        );
        assert_eq!(output.stdout, b"");
        assert!(!self.path("ledger.jsonl").exists());
        assert_eq!(self.listing(), before);
    }
}

#[test]
fn worktree_deleted_entry_accepts_external_output_without_pruning() {
    let session = Session::new(true);
    let before = session.listing();
    assert!(String::from_utf8_lossy(&before).contains("prunable"));
    let output = session.device(&session.path("artifacts"));
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, b"");
    assert_eq!(session.report()["result"], "pass");
    assert_eq!(session.report()["checks"]["session_released"], true);
    assert_eq!(session.listing(), before);
    assert!(session.live.is_dir());
    assert!(!session.stale.exists());
}

#[test]
fn worktree_live_main_and_linked_descendants_are_refused_even_after_stale_record() {
    for stale in [false, true] {
        let session = Session::new(stale);
        for root in [&session.repo, &session.live] {
            let target = root.join("artifacts");
            session.refusal(&target, "artifacts must be outside all repository worktrees");
            assert!(!target.exists());
        }
    }
}

#[test]
fn worktree_independent_git_ancestor_is_refused() {
    let session = Session::new(false);
    let other = session.path("independent");
    fs::create_dir(&other).unwrap();
    fs::write(other.join(".git"), b"synthetic git marker").unwrap();
    let target = other.join("artifacts");
    session.refusal(&target, "artifacts must be outside git");
    assert!(!target.exists());
}

#[test]
fn worktree_existing_destination_and_missing_parent_keep_diagnostics() {
    let session = Session::new(false);
    let target = session.path("existing");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("keep"), b"original").unwrap();
    session.refusal(&target, "destination already exists");
    assert_eq!(fs::read(target.join("keep")).unwrap(), b"original");
    let missing = session.path("missing/out");
    session.refusal(&missing, "parent must exist");
    assert!(!missing.parent().unwrap().exists());
}

fn m5_prelaunch(action: &str, diagnostic: &str) {
    let session = Session::new(true);
    session.scenario("M5", &json!([{"mcp":{"action":action}}]));
    let before = session.listing();
    let output = session
        .command()
        .args([
            "--driver",
            "sim",
            "--bridge",
            env!("CARGO_BIN_EXE_echo-lab-sim-script"),
            "--out",
            "artifacts",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!("echo-lab: scenario.yaml: step 0: M5: {diagnostic}\n")
    );
    let report = session.report();
    assert_eq!(report["result"], "fail");
    assert_eq!(report["checks"], json!({"step_0":false,"world_closed":true}));
    assert!(report.get("link_counts").is_none());
    assert!(!session.path("artifacts/mcp.private").exists());
    assert!(
        !fs::read_to_string(session.path("artifacts/events.jsonl"))
            .unwrap()
            .contains("sim.endpoint")
    );
    assert_eq!(session.listing(), before);
}

#[test]
fn worktree_m5_restart_keeps_prelaunch_diagnostic() {
    m5_prelaunch("restart", "restart requires a running linked bridge");
}

#[test]
fn worktree_m5_call_and_probe_keep_prelaunch_diagnostic() {
    for action in ["call", "probe"] {
        m5_prelaunch(action, "resource control requires a running linked bridge");
    }
}

#[test]
fn worktree_m5_replay_keeps_prelaunch_diagnostic() {
    m5_prelaunch("replay", "replay requires a live rotated grant");
}
