//! T3 CLI admission: controlled tools, no sockets, accounts or hardware.
use crate::run_support::{FAKE, Session};
use serde_json::json;

#[test]
fn device_unsafe_or_unreadable_config_never_invokes_bench_tools() {
    let original = include_str!("../../../../examples/bridge.example.toml")
        .replace("echo.example.invalid", "127.0.0.1")
        .replace("0.0.0.0:16900", "127.0.0.1:0"); // lint-rules: allow-literal inert template substitution
    for template in [
        None,
        Some("malformed TOML".into()),
        Some(original.replace("127.0.0.1", "localhost")),
        Some(original.replace("http://127.0.0.1:18080/mcp", "https://mcp.example.invalid/mcp")),
        Some(original.replace("auth = \"none\"", "auth = \"bearer_file\"")),
    ] {
        let session = Session::new(&json!({}));
        if let Some(template) = template {
            std::fs::write(session.0.join("bridge.toml"), template).unwrap();
        }
        let output = session
            .command()
            .args([
                "run",
                "one.yaml",
                "--driver",
                "device",
                "--bench",
                "ring",
                "--port",
                FAKE,
                "--adb",
                FAKE,
                "--stt",
                FAKE,
                "--bridge",
                FAKE,
                "--bridge-config",
                "./bridge.toml",
                "--python",
                if cfg!(windows) { "python" } else { "python3" },
                "--out",
                "artifacts",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(session.operations(), Vec::<String>::new());
        assert!(!session.0.join("artifacts").exists());
    }
}

#[test]
fn device_typed_ui_requires_pair_but_golden_dispatch_is_preserved() {
    let session = Session::new(&json!({}));
    session.scenario(
        "one.yaml",
        &json!([{"expect_screen":{"state":"ready","message":"Echo Live ready"}}]),
    );
    let path = session.0.join("one.yaml");
    let scenario = std::fs::read_to_string(&path).unwrap().replace("\"B0\"", "\"M1\"");
    std::fs::write(path, scenario).unwrap();
    let output = session.run(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("require --bridge and --bridge-config"));
    assert_eq!(session.operations(), Vec::<String>::new());
    assert!(!session.0.join("artifacts").exists());
}
