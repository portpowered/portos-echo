use super::*;

fn scan(path: &str, source: &str) -> (Vec<String>, usize) {
    check(&BTreeMap::from([(path.into(), source.into())]))
}

fn lines(errors: &[String]) -> Vec<usize> {
    errors
        .iter()
        .map(|e| e.split(':').nth(1).unwrap().parse().unwrap())
        .collect()
}

#[test]
fn hosts_are_exact_in_urls_sockets_and_embedded_serialization() {
    for literal in [
        "http://127.0.0.1:80/path",
        "wss://localhost/",
        "https://[::1]:443/", // lint-rules: allow-literal parser safe URL cases
        "127.0.0.1:0",
        "[::1]:16900",
        "localhost:80",
    ] {
        let source = format!("const INPUT: &str = {literal:?};");
        assert_eq!(scan("tests/unit.rs", &source), (vec![], 0), "{literal}");
    }
    for literal in [
        "https://api.example.com", // lint-rules: allow-literal network scanner parser rejection fixture
        "HTTP://remote.invalid/path", // lint-rules: allow-literal network scanner parser rejection fixture
        "ftp://host",              // lint-rules: allow-literal parser remote URL cases
        "http://localhost.evil",   // lint-rules: allow-literal network scanner parser rejection fixture
        "http://127.0.0.1@remote.invalid", // lint-rules: allow-literal parser misleading host cases
        "http://[::1]junk",        // lint-rules: allow-literal network scanner parser rejection fixture
        "http://127.0.0.2",        // lint-rules: allow-literal network scanner parser rejection fixture
        "https://[2001:db8::1]/",  // lint-rules: allow-literal parser IPv6 and near loopback cases
        "0.0.0.0:0",               // lint-rules: allow-literal network scanner parser rejection fixture
        "[::]:80",                 // lint-rules: allow-literal network scanner parser rejection fixture
        "remote.invalid:443",      // lint-rules: allow-literal parser socket cases
        "{\"url\":\"https://remote.invalid/x\"}", // lint-rules: allow-literal network scanner parser rejection fixture
        "listen = 'host.invalid:80'", // lint-rules: allow-literal parser embedded fixture cases
        "λ host.invalid:80",       // lint-rules: allow-literal parser Unicode byte offsets
    ] {
        let source = format!("const INPUT: &str = {literal:?};");
        let (errors, count) = scan("tests/unit.rs", &source);
        assert_eq!(lines(&errors), [1], "{literal}");
        assert_eq!(count, 0);
        assert!(errors[0].contains(": network-tag:"));
        assert!(!errors[0].contains(literal));
    }
}

#[test]
fn decoded_raw_escaped_multiline_literals_report_physical_lines() {
    let source = concat!(
        "const A: &str = r###\"{\"url\":\"https://remote.invalid\"}\"###;\n", // lint-rules: allow-literal parser raw source fixture
        "const B: &str = \"https:\\x2f\\u{2f}remote.invalid\";\n",
        "const C: &str = \"prefix\nhttps://remote.invalid\";\n", // lint-rules: allow-literal parser multiline source fixture
        "const D: &str = b\"[::]:80\";\n",                       // lint-rules: allow-literal parser byte source fixture
        "const E: &str = \"https://\\\n remote.invalid\";\n", // lint-rules: allow-literal parser continuation fixture
    );
    assert_eq!(lines(&scan("tests/unit.rs", source).0), [1, 2, 4, 5, 6]);
}

#[test]
fn cloud_exemption_requires_both_exact_path_and_first_source_line() {
    let body = "const URL: &str = \"https://remote.invalid\";"; // lint-rules: allow-literal parser cloud source fixture
    let gate = "#![cfg(feature = \"cloud-tests\")]";
    for path in ["tests/cloud_api.rs", "crates/demo/tests/cloud_api.rs"] {
        assert_eq!(scan(path, &format!("{gate}\n{body}")), (vec![], 0));
        for prefix in [
            "",
            "// gate\n",
            "/* #![cfg(feature = \"cloud-tests\")] */\n",
            "const FAKE: &str = \"#![cfg(feature = \\\"cloud-tests\\\")]\";\n",
        ] {
            assert_eq!(scan(path, &format!("{prefix}{body}")).0.len(), 1);
        }
    }
    for path in ["tests/api.rs", "crates/demo/src/cloud_api.rs", "contest/cloud_api.rs"] {
        let source = format!("{gate}\n#[test] fn check() {{ {body} }}");
        assert_eq!(lines(&scan(path, &source).0), [2]);
    }
}

#[test]
fn annotations_are_real_reasoned_comments_on_matching_lines_and_counted() {
    let remote = "\"https://remote.invalid\""; // lint-rules: allow-literal parser annotation source fixture
    let marker = "// lint-rules: allow-literal serialization assertion fixture";
    let source =
        format!("const A: &str = {remote}; {marker}\nconst B: &str = {remote}; {marker}\nconst C: &str = {remote};");
    let (errors, count) = scan("tests/unit.rs", &source);
    assert_eq!(lines(&errors), [3]);
    assert_eq!(count, 2);
    for (prefix, suffix, expected_line) in [
        ("", "// lint-rules: allow-literal ", 1),
        ("// lint-rules: allow-literal previous assertion\n", "", 2),
        ("", "/* // lint-rules: allow-literal fake assertion */", 1),
        (
            "const FAKE: &str = \"// lint-rules: allow-literal fake assertion\";\n",
            "",
            2,
        ),
    ] {
        let (errors, count) = scan("tests/unit.rs", &format!("{prefix}const A: &str = {remote}; {suffix}"));
        assert_eq!(lines(&errors), [expected_line]);
        assert_eq!(count, 0);
    }
    assert_eq!(
        scan("tests/unit.rs", "// lint-rules: allow-literal unused assertion"),
        (vec![], 0)
    );
}

#[test]
fn test_regions_include_helpers_macros_and_external_modules_but_not_product_code() {
    let remote = "\"https://remote.invalid\""; // lint-rules: allow-literal parser source-context fixture
    let mut sources = BTreeMap::from([
        (
            "crates/demo/src/lib.rs".into(),
            format!(
                "const PROD: &str = {remote};\n#[cfg(test)]\n#[path = \"checks.rs\"]\nmod checks;\n#[test] fn inline() {{ macro_call!({remote}); }}\n#[cfg(test)] mod inside {{ fn helper() {{ let _ = {remote}; }} mod nested; }}"
            ),
        ),
        (
            "crates/demo/src/checks.rs".into(),
            format!("fn helper() {{ let _ = {remote}; }} mod support;"),
        ),
        (
            "crates/demo/src/checks/support.rs".into(),
            format!("const HELPER: &str = {remote};"),
        ),
        (
            "crates/demo/src/inside/nested.rs".into(),
            format!("const NESTED: &str = {remote};"),
        ),
        (
            "crates/demo/src/product.rs".into(),
            format!("const PROD: &str = {remote};"),
        ),
    ]);
    let (errors, count) = check(&sources);
    assert_eq!(errors.len(), 5, "{errors:?}");
    assert_eq!(count, 0);
    assert!(!errors.iter().any(|e| e.contains("product.rs")));
    assert_eq!(
        lines(
            &scan(
                "src/lib.rs",
                &format!("const PROD: &str = {remote};\n#[test]\nfn test() {{ let _ = {remote}; }}")
            )
            .0
        ),
        [3]
    );
    sources.insert(
        "src/test_only.rs".into(),
        format!("#![cfg(test)]\nconst HELPER: &str = {remote};"),
    );
    assert_eq!(check(&sources).0.len(), 6);
}

#[test]
fn comments_and_syntax_in_strings_do_not_create_test_regions() {
    let source = "// #[test]\n/* #[cfg(test)] mod x { */\nconst PROD: &str = \"https://remote.invalid\";\nconst FAKE: &str = \"#[cfg(test)] mod x {}\";"; // lint-rules: allow-literal parser comment source fixture
    assert_eq!(scan("src/lib.rs", source), (vec![], 0));
    assert_eq!(
        scan(
            "tests/unit.rs",
            "// https://remote.invalid\n/* https://remote.invalid */" // lint-rules: allow-literal network scanner parser rejection fixture
        ),
        (vec![], 0)
    ); // lint-rules: allow-literal parser comment fixture
}

#[test]
fn include_helpers_visibility_and_same_line_product_regions_are_precise() {
    let remote = "\"https://remote.invalid\""; // lint-rules: allow-literal parser helper source fixture
    let sources = BTreeMap::from([
        (
            "src/lib.rs".into(),
            "#[cfg(test)] #[path = \"check.rs\"] pub(crate) mod check;".into(),
        ),
        ("src/check.rs".into(), "include!(\"included.rs\");".into()),
        ("src/included.rs".into(), format!("fn helper() {{ let _ = {remote}; }}")),
    ]);
    let (errors, count) = check(&sources);
    assert_eq!(lines(&errors), [1]);
    assert!(errors[0].starts_with("src/included.rs:"));
    assert_eq!(count, 0);
    let source = format!("const PROD: &str = {remote}; #[test] fn unit() {{ let _ = \"localhost:80\"; }}");
    assert_eq!(scan("src/lib.rs", &source), (vec![], 0));
    let source = format!("#[tokio::test] async fn unit() {{ let _ = {remote}; }}");
    assert_eq!(lines(&scan("src/lib.rs", &source).0), [1]);
}

#[test]
fn configured_test_attributes_select_bodies_without_selecting_product_cfg() {
    let remote = "\"https://api.example.com\""; // lint-rules: allow-literal scanner attribute source fixture
    for attribute in [
        "#[tokio::test(flavor = \"current_thread\")]",
        "#[tokio::test(start_paused = true)]",
        "#[test]",
        "#[cfg(test)]",
        "#[cfg(all(test, feature = \"checks\"))]",
        "#[cfg(not(not(test)))]",
        "#[cfg(any(test, feature = \"checks\"))]",
    ] {
        let source = format!("{attribute}\nasync fn check() {{ let _ = {remote}; }}");
        let (errors, count) = scan("src/lib.rs", &source);
        assert_eq!(lines(&errors), [2], "{attribute}");
        assert_eq!(count, 0);
    }
    for gate in ["not(test)", "all(not(test), unix)", "not(any(test, unix))"] {
        let source = format!("#[cfg({gate})]\nfn product() {{ let _ = {remote}; }}");
        assert_eq!(scan("src/lib.rs", &source), (vec![], 0), "{gate}");
        let source = format!("#![cfg({gate})]\nconst URL: &str = {remote};");
        assert_eq!(scan("src/lib.rs", &source), (vec![], 0), "{gate}");
    }
}
