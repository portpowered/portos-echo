//! T3: the app server's own tools beyond the bridge's catalog do nothing here.
//!
//! No Codex release is pinned, so a release may declare tools the bridge never
//! asked for. They are not refused by name. This witness calls each one the
//! running app server declares and shows what holds for any release: the
//! thread's sandbox is read-only, so nothing is written, and its configuration
//! names no MCP server, so the resource helpers have nothing to return.
use super::*;

#[tokio::test]
async fn t3_pinned_tools_native_extras_are_inert() {
    tokio::time::timeout(Duration::from_secs(25), Box::pin(exercise()))
        .await
        .unwrap();
}

async fn exercise() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("native-extra.txt");
    let config = CodexConfig {
        binary: Some(binary()),
        home: Some(directory.path().join("home")),
        ..CodexConfig::default()
    };
    let spoof = Spoof::start(echo_spoof_mcp::Options::default()).await.unwrap();
    let (host, catalog) = catalog(&spoof).await;
    let mut provider = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pinned_media/provider.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = provider.stdin.take().unwrap();
    let mut output = BufReader::new(provider.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    let origin = ready["origin"].as_str().unwrap();
    let routes = LocalRoutes::new(origin, origin).unwrap();
    let initialize = json!({"tools":catalog.tools(), "native_extras":target});
    input.write_all(format!("{initialize}\n").as_bytes()).await.unwrap();
    let (commands, receive) = mpsc::channel(2);
    let (started, model_started) = tokio::sync::oneshot::channel();
    let budget = ToolBudget::new(1, || Ok(7));
    let flags = (false, false, model_started, budget, "gpt-6.1-sol");
    let ((), (mut remote, _)) = tokio::join!(
        client(&config, &routes, (host, catalog), commands, flags),
        remote(&mut input, &mut output, receive, started, &MODEL_STARTED)
    );
    remote.close();
    input.write_all(b"{\"snapshot\":true}\n").await.unwrap();
    let value: Value = serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
    // The observed catalog, names only, is the evidence a targeted run records.
    writeln!(
        std::io::stdout(),
        "{}",
        json!({"catalog":value["catalog"],"extras":value["extras"]})
    )
    .unwrap();
    check_extras(&value, &target);
    // The bridge's own tool still ran exactly once, after the probes.
    assert_eq!(value["appends"], json!(["delegation-local"]));
    check_mcp(&spoof, false);
    let requests = spoof.snapshot().unwrap().requests;
    assert!(requests.iter().all(|request| !request.method.starts_with("resources/")));
    drop(input);
    assert!(provider.wait().await.unwrap().success());
    spoof.shutdown().await.unwrap();
}

fn check_extras(value: &Value, target: &Path) {
    let known = [
        "functions.apply_patch",
        "functions.list_mcp_resources",
        "functions.list_mcp_resource_templates",
        "functions.read_mcp_resource",
    ];
    let declared = value["catalog"]["extras"].as_array().unwrap();
    let probed = value["extras"].as_object().unwrap();
    // Every one of these the running release declares was called.
    for name in known.iter().filter(|name| declared.contains(&json!(name))) {
        assert!(probed.contains_key(*name), "{name} was declared and not probed");
    }
    for (name, output) in probed {
        let output = output.as_str().unwrap();
        let parsed = serde_json::from_str::<Value>(output).ok();
        match name.as_str() {
            "functions.apply_patch" => assert!(!output.contains("Success"), "{output}"),
            "functions.list_mcp_resources" => assert_eq!(parsed, Some(json!({"resources":[]}))),
            "functions.list_mcp_resource_templates" => assert_eq!(parsed, Some(json!({"resourceTemplates":[]}))),
            // A refusal is prose; a resource would be a JSON object.
            "functions.read_mcp_resource" => assert_eq!(parsed, None, "{output}"),
            name => assert_eq!(name, "", "unexpected probe"),
        }
    }
    assert!(!target.exists(), "a native tool wrote a file");
    let homes = ["home", "home-api-key"].map(|name| target.with_file_name(name));
    assert!(homes.iter().all(|home| home.is_dir()));
    assert_eq!(
        std::fs::read_dir(target.parent().unwrap()).unwrap().count(),
        homes.len(),
        "only the configured home and API-key sibling"
    );
}
