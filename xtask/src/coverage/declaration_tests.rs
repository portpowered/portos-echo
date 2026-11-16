use super::*;

fn changes(paths: &[&str]) -> Changed {
    paths.iter().map(|path| ((*path).into(), BTreeSet::from([1]))).collect()
}

fn expected(paths: &[&str], changed: &str) -> Vec<String> {
    let mut messages: Vec<String> = [
        "workspace lines: 4/5 (80.00%), floor 80%",
        "workspace branches: 3/4 (75.00%), floor 70%",
        "xtask lines: 4/5 (80.00%), floor 0%",
        "xtask branches: 3/4 (75.00%), floor 0%",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    messages.extend(paths.iter().map(|path| format!("coverage: no executable code: {path}")));
    messages.push(changed.into());
    messages
}

fn crates() -> Vec<(String, String)> {
    vec![("xtask".into(), "xtask/".into())]
}

#[test]
fn coverage_declarations_emit_exact_messages_and_read_once() {
    let path = "tests/cases/mod.rs";
    let mut reads = Vec::new();
    let result = evaluate(&tests::report(), &changes(&[path]), &crates(), |path| {
        reads.push(path.to_owned());
        Ok("mod a;\nmod b;\npub(crate) mod c;".into())
    });
    assert_eq!(
        result,
        Ok(expected(
            &[path],
            "changed executable Rust lines: 0/0 (100.00%), floor 85%"
        ))
    );
    assert_eq!(reads, [path]);
}

#[test]
fn coverage_lexical_declarations_ignore_comments_and_literals() {
    for path in ["tests/cases/mod.rs", "src/items.rs"] {
        for source in [
            "",
            "// fn x",
            "/* fn /* fn */ fn */",
            r#"const S: &str = "fn";"#,
            r#"const S: &str = "escaped \" fn";"#,
            r##"const S: &str = r#"fn"#;"##,
            r##"const S: &[u8] = br#"fn"#;"##,
            r#"const S: &[u8] = b"fn";"#,
            r#"const S: &CStr = c"fn";"#,
            r##"const S: &CStr = cr#"fn"#;"##,
            "use a::B; pub use c::D; struct S; enum E { A } const X: u8 = 1; type T = S;",
            "mod a; pub(crate) mod b; const fnx: u8 = 1; struct my_fn;",
        ] {
            assert!(no_executable_code(path, source), "{path}: {source}");
        }
    }
    assert!(!no_executable_code("src/items.txt", "mod a;"));
}

#[test]
fn coverage_any_fn_token_is_executable_regardless_of_path() {
    for path in ["tests/cases/mod.rs", "src/items.rs"] {
        for source in [
            "fn helper() {}",
            "pub const fn x() {}",
            "async fn x() {}",
            "impl S { fn x() {} }",
            "trait T { fn x(); }",
            "type F = fn();",
            "macro_rules! m { () => { fn x() {} } }",
        ] {
            assert!(!no_executable_code(path, source), "{path}: {source}");
            assert_eq!(
                evaluate(&tests::report(), &changes(&[path]), &crates(), |_| Ok(source.into())),
                Err(format!("changed Rust source missing from coverage: {path}")),
            );
        }
    }
}

#[test]
fn coverage_head_reader_failure_is_exact_missing_source_error() {
    let path = "src/items.rs";
    let mut reads = Vec::new();
    assert_eq!(
        evaluate(&tests::report(), &changes(&[path]), &crates(), |path| {
            reads.push(path.to_owned());
            Err("HEAD blob unavailable".into())
        }),
        Err(format!("changed Rust source missing from coverage: {path}"))
    );
    assert_eq!(reads, [path]);
}

#[test]
fn coverage_covered_paths_and_empty_ranges_do_not_read_head() {
    let mut changes = changes(&["xtask/src/main.rs"]);
    changes.insert("src/empty.rs".into(), BTreeSet::new());
    let mut reads = Vec::new();
    let result = evaluate(&tests::report(), &changes, &crates(), |path| {
        reads.push(path.to_owned());
        Err("must not read".into())
    });
    assert_eq!(
        result,
        Ok(expected(&[], "changed executable Rust lines: 1/1 (100.00%), floor 85%"))
    );
    assert_eq!(reads, Vec::<String>::new());
}

#[test]
fn coverage_multiple_declarations_keep_order_and_executable_counts() {
    let mut reads = Vec::new();
    let result = evaluate(
        &tests::report(),
        &changes(&["src/b.rs", "src/a.rs"]),
        &crates(),
        |path| {
            reads.push(path.to_owned());
            Ok("struct S;".into())
        },
    );
    assert_eq!(
        result,
        Ok(expected(
            &["src/a.rs", "src/b.rs"],
            "changed executable Rust lines: 0/0 (100.00%), floor 85%"
        ))
    );
    assert_eq!(reads, ["src/a.rs", "src/b.rs"]);
}

#[test]
fn coverage_declarations_cannot_mask_existing_floor_failures() {
    let mut report = tests::report();
    let mut changed = changes(&["src/items.rs"]);
    changed.insert("xtask/src/main.rs".into(), BTreeSet::from([5]));
    let result = evaluate(&report, &changed, &crates(), |_| Ok("mod a;".into()));
    assert_eq!(
        result,
        Err("changed executable Rust lines: 0/1 (0.00%), floor 85%".into())
    );
    let high_floor = [("echo-contracts".into(), "xtask/".into())];
    assert_eq!(
        evaluate(&report, &changed, &high_floor, |_| Ok("mod a;".into())),
        Err("echo-contracts lines: 4/5 (80.00%), floor 95%".into())
    );
    report
        .get_mut("xtask/src/main.rs")
        .unwrap()
        .branches
        .insert((2, 0, 0), false);
    assert_eq!(
        evaluate(&report, &changed, &crates(), |_| Ok("mod a;".into())),
        Err("workspace branches: 2/4 (50.00%), floor 70%".into())
    );
}
