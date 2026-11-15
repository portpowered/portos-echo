use super::*;

const PASS: &str =
    "running 1 test\ntest schema_snapshots ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 1 filtered out\n";

#[test]
fn both_real_witness_commands_are_read_only_and_exact() {
    let packages = WITNESSES.iter().map(|witness| witness.package.to_owned()).collect();
    let mut calls = Vec::new();
    let mut inputs = Vec::new();
    check(
        &packages,
        "",
        |path| {
            inputs.push(path.to_owned());
            Ok(())
        },
        |command| {
            assert_eq!(command.get_program(), "cargo");
            let arguments: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            calls.push(arguments);
            let environment: Vec<_> = command.get_envs().collect();
            assert!(environment.contains(&(std::ffi::OsStr::new("UPDATE_SCHEMAS"), None)));
            Ok((true, PASS.into()))
        },
    )
    .unwrap();
    for (index, witness) in WITNESSES.iter().enumerate() {
        assert_eq!(
            calls[index],
            [
                "test",
                "--locked",
                "--offline",
                "-p",
                witness.package,
                "--test",
                witness.target,
                "schema_snapshots",
                "--",
                "--exact"
            ]
        );
        assert_eq!(inputs[index * 2], format!("crates/{}/Cargo.toml", witness.package));
        assert_eq!(
            inputs[index * 2 + 1],
            format!("crates/{}/tests/{}.rs", witness.package, witness.target)
        );
    }
}

#[test]
fn tracked_sources_and_orphan_schema_families_require_witnesses() {
    for witness in WITNESSES {
        for path in [
            format!("crates/{}/src/lib.rs", witness.package),
            format!("schemas/{}/missing.json", witness.family),
        ] {
            let error = check(
                &Names::new(),
                &path,
                |_| Err(std::io::ErrorKind::NotFound.into()),
                |_| unreachable!(),
            )
            .unwrap_err();
            assert!(
                error.starts_with(&format!(
                    "crates/{}/Cargo.toml:1: schema witness input:",
                    witness.package
                )),
                "{error}"
            );
        }
    }
    assert_eq!(
        check(
            &Names::new(),
            "echo-fixture/src/main.rs\0",
            |_| unreachable!(),
            |_| unreachable!()
        ),
        Ok(())
    );
}

#[test]
fn missing_witness_source_and_spawn_failures_are_not_equality() {
    let packages = ["echo-contracts".to_owned()].into_iter().collect();
    let source_error = check(
        &packages,
        "",
        |path| {
            if path == "crates/echo-contracts/tests/contracts_schema.rs" {
                Err(std::io::ErrorKind::NotFound.into())
            } else {
                Ok(())
            }
        },
        |_| unreachable!(),
    )
    .unwrap_err();
    assert!(source_error.starts_with("crates/echo-contracts/tests/contracts_schema.rs:1: schema witness input:"));
    let spawn_error = check(
        &packages,
        "",
        |_| Ok(()),
        |_| Err(std::io::ErrorKind::PermissionDenied.into()),
    )
    .unwrap_err();
    assert!(
        spawn_error.starts_with("crates/echo-contracts/tests/contracts_schema.rs:1: schema witness could not start:")
    );
}

#[test]
fn matching_missing_and_stale_results_preserve_schema_diagnostics() {
    for witness in WITNESSES {
        assert_eq!(outcome(witness, true, PASS), Ok(()));
        for message in [
            format!(
                "Error: schema mismatch: /workspace/crates/{}/../../schemas/{}/event.json",
                witness.package, witness.family
            ),
            format!(
                "Error: C:\\workspace\\schemas\\{}\\event.json: missing; use UPDATE_SCHEMAS=1",
                witness.family
            ),
            format!(
                "Error: \"schema mismatch: C:\\\\workspace\\\\schemas/{}\\\\event.json\"",
                witness.family
            ),
        ] {
            let error = outcome(witness, false, &message).unwrap_err();
            assert!(
                error.starts_with(&format!("schemas/{}/event.json:1: schema-drift:", witness.family)),
                "{error}"
            );
            assert!(error.ends_with(&message));
        }
    }
}

#[test]
fn compile_or_zero_test_success_cannot_prove_schema_equality() {
    for (success, output) in [
        (false, "error[E0425]: cannot find value"),
        (true, "running 0 tests\ntest result: ok. 0 passed; 0 failed;"),
        (
            true,
            "test schema_snapshots ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored;",
        ),
        (false, PASS),
    ] {
        let error = outcome(WITNESSES[0], success, output).unwrap_err();
        assert!(error.starts_with("crates/echo-contracts/tests/contracts_schema.rs:1: schema witness failed or did not execute schema_snapshots"));
        assert!(error.ends_with(output));
    }
}
