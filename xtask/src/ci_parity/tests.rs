use super::*;

fn fixture() -> (String, String) {
    let makefile = format!(
        "ci: {}\n{}",
        GATES.iter().map(|(_, t)| *t).collect::<Vec<_>>().join(" "),
        shards::MAKE_SPLITS.concat()
    );
    let jobs: String = canonical().into_iter().map(|(_, block)| block).collect();
    (makefile, format!("{PREFIX}{jobs}"))
}
fn assert_failure(makefile: &str, workflow: &str, reason: &str) {
    let error = evaluate(makefile, workflow).unwrap_err();
    assert!(error.contains(reason), "{error}");
}
fn assert_job_rejected(workflow: &str, block: &str, changed: &str, name: &str) {
    let (makefile, _) = fixture();
    assert_eq!(workflow.matches(block).count(), 1, "{name}");
    let error = evaluate(&makefile, &workflow.replace(block, changed)).unwrap_err();
    assert!(error.contains(name), "{name}: {error}");
}
#[test]
fn workflow_contexts_rejects_job_env_and_preserves_step_scope() {
    let fixed = include_str!("../../../.github/workflows/build-codex-armv7.yml");
    for newline in ["\n", "\r\n"] {
        assert_eq!(workflow_contexts("arm.yml", &fixed.replace('\n', newline)), Ok(()));
        for expression in [
            "${{ runner.temp }}",
            "${{runner.temp}}",
            "${{ runner . temp }}",
            "${{ runner['temp'] }}",
        ] {
            for key in ["CODEX_ARMV7_WORK_DIR", "CODEX_ARMV7_OUTPUT_DIR"] {
                let yaml = format!("jobs:\n  build:\n    env:\n      {key}: {expression}\n").replace('\n', newline);
                assert_eq!(
                    workflow_contexts("arm.yml", &yaml),
                    Err("arm.yml:4: runner context is unavailable in job-level env".into())
                );
            }
        }
    }
    let allowed = "jobs:\n  build:\n    env:\n      TEXT: runner.temp\n      QUOTED: ${{ 'runner.temp' }}\n      OTHER: ${{ github . runner . temp }}\n      # COMMENT: ${{ runner.temp }}\n      SAFE: literal # ${{ runner.temp }}\n    steps:\n      - run: |\n          env:\n            TEXT: ${{ runner.temp }}\n        env:\n          DIR: ${{ runner.temp }}\n        with:\n          path: ${{ runner.temp }}/output\n  next:\n    steps:\n      - run: true\n        env:\n          DIR: ${{ runner.temp }}\n";
    assert_eq!(workflow_contexts("arm.yml", allowed), Ok(()));
    let invalid = format!("{allowed}    env:\n      DIR: ${{{{ runner.temp }}}}\n");
    assert!(
        workflow_contexts("arm.yml", &invalid)
            .unwrap_err()
            .contains("runner context is unavailable")
    );
}
#[test]
fn matching_ordered_gates_pass() {
    let (makefile, workflow) = fixture();
    assert_eq!(evaluate(&makefile, &workflow), Ok(()));
}
#[test]
fn split_gates_require_every_part_and_fail_when_any_part_fails() {
    let (_, workflow) = fixture();
    for (gate, parts) in shards::SPLIT {
        let block = job(gate, gate);
        for part in parts {
            let needs = format!("needs: [{}]", parts.join(", "));
            let fewer = parts
                .iter()
                .filter(|p| *p != part)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            let check = format!("test '${{{{ needs.{part}.result }}}}' = success");
            for changed in [
                block.replace(&needs, &format!("needs: [{fewer}]")),
                block.replace(&check, "true"),
            ] {
                assert_job_rejected(&workflow, &block, &changed, gate);
            }
            assert_job_rejected(&workflow, &shards::part(part), "", part);
        }
        for (from, to) in [
            ("    if: always()\n", ""),
            ("    if: always()\n", "    if: success()\n"),
            ("= success", "!= failure"),
            ("= success", "= success || true"),
        ] {
            assert_job_rejected(&workflow, &block, &block.replace(from, to), gate);
        }
    }
}
#[test]
fn shards_cover_every_partition_exactly_once() {
    let (_, workflow) = fixture();
    for (part, from, to) in [
        ("test-unit-rust", "shard: [1, 2, 3, 4]", "shard: [1, 2, 3]"),
        (
            "test-unit-rust",
            "count:${{ matrix.shard }}/4",
            "count:${{ matrix.shard }}/5",
        ),
        ("test-unit-rust", "      fail-fast: false\n", ""),
        ("test-sim-host-shards", "shard: [1, 2, 3]", "shard: [1, 2]"),
        (
            "test-sim-host-shards",
            "count:${{ matrix.shard }}/3",
            "hash:${{ matrix.shard }}/3",
        ),
        ("test-sim-suites-parts", ", test-sim-components]", "]"),
        (
            "test-sim-suites-parts",
            "make ${{ matrix.part }}",
            "make ${{ matrix.part }} || true",
        ),
        (
            "test-unit-screen",
            "make test-unit-python test-unit-screen",
            "make test-unit-screen",
        ),
        (
            "build-host-workspace",
            "make build-host-workspace",
            "make build-host-workspace || true",
        ),
        (
            "test-echo-install",
            "make test-echo-install",
            "make test-echo-install || true",
        ),
    ] {
        let block = shards::part(part);
        assert_job_rejected(&workflow, &block, &block.replace(from, to), part);
    }
    let (makefile, workflow) = fixture();
    for rule in shards::MAKE_SPLITS {
        for changed in [
            String::new(),
            format!("{rule}{rule}"),
            rule.replace(" $(NEXTEST_SHARD)", ""),
        ] {
            assert_failure(
                &makefile.replace(rule, &changed),
                &workflow,
                "Makefile: split gate differs",
            );
        }
    }
    for (from, to) in [
        (
            "test-unit: test-unit-rust test-unit-python test-unit-screen",
            "test-unit: test-unit-rust",
        ),
        (
            "build-host: build-host-workspace test-echo-install",
            "build-host: build-host-workspace",
        ),
        (
            "--partition $(NEXTEST_PARTITION) --no-tests=pass",
            "--partition $(NEXTEST_PARTITION)",
        ),
    ] {
        assert_failure(&makefile.replace(from, to), &workflow, "Makefile: split gate differs");
    }
}
#[test]
fn codex_provisioning_cannot_be_removed_or_made_optional() {
    let (_, workflow) = fixture();
    let block = shards::part("test-unit-rust");
    for replacement in ["", "        run: sh scripts/codex-app-server.sh || true\n"] {
        let changed = block.replace("        run: sh scripts/codex-app-server.sh\n", replacement);
        assert_job_rejected(&workflow, &block, &changed, "test-unit-rust");
    }
    let namespace = "        run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0\n";
    for replacement in [String::new(), format!("{namespace}        if: false\n")] {
        assert_job_rejected(
            &workflow,
            &block,
            &block.replace(namespace, &replacement),
            "test-unit-rust",
        );
    }
    for part in ["test-sim-suites-parts", "test-sim-host-shards"] {
        let block = shards::part(part);
        assert_job_rejected(&workflow, &block, &block.replace("          sim: 'true'\n", ""), part);
    }
}
#[test]
fn echo_gate_requires_exact_provisioning_build_and_publication() {
    let (makefile, workflow) = fixture();
    let echo = job("build-echo", "build-echo");
    for changed in [
        String::new(),
        format!("{echo}{echo}"),
        echo.replace("    name: build-echo", "    if: false\n    name: build-echo"),
        echo.replace("run: make build-echo", "run: make build-echo || true"),
        echo.replace("armv7-linux-androideabi", "aarch64-linux-android"),
        echo.replace("rustup target add --toolchain", "rustup target add"),
        echo.replace("          if-no-files-found: error\n", ""),
        echo.replace("crates/echo-bridge/Cargo.toml", "crates/other/Cargo.toml"),
        echo.replace("target/armv7-linux-androideabi/release/echo-bridge", "target/stale"),
    ] {
        assert!(evaluate(&makefile, &workflow.replace(&echo, &changed)).is_err());
    }
    assert!(evaluate(&makefile.replace(" build-echo", ""), &workflow).is_err());
    let reordered = workflow.replace(
        &(job("build-host", "build-host") + &echo),
        &(echo + &job("build-host", "build-host")),
    );
    assert!(evaluate(&makefile, &reordered).is_err());
}
#[test]
fn missing_extra_duplicate_and_reordered_gates_fail() {
    let (makefile, workflow) = fixture();
    for (input, diagnostic) in [
        (makefile.replace("lint-shape ", ""), "missing gate lint-shape"),
        (format!("{makefile}ci: other\n"), "exactly one"),
        (
            makefile.replacen("build-host", "build-host surprise", 1),
            "extra or unsupported gate surprise",
        ),
        (
            makefile.replacen("build-host", "build-host build-host", 1),
            "duplicate gate build-host",
        ),
        (
            makefile.replace("test-unit test-contract", "test-contract test-unit"),
            "gate order differs",
        ),
    ] {
        assert_failure(&input, &workflow, diagnostic);
    }
    let missing = workflow.replace(&job("lint-shape", "lint-shape"), "");
    assert_failure(&makefile, &missing, "missing gate lint-shape");
    let duplicate = format!("{workflow}{}", job("format", "fmt-check"));
    assert_failure(&makefile, &duplicate, "duplicate gate format");
    let extra = format!("{workflow}{}", job("extra", "extra"));
    assert_failure(&makefile, &extra, "unsupported gate extra");
    // The owner suspended coverage on 2026-11-18; it is no longer a CI gate.
    assert_failure(
        &makefile.replacen("build-host", "coverage build-host", 1),
        &workflow,
        "extra or unsupported gate coverage",
    );
    let reordered = workflow.replace(
        &(job("test-unit", "test-unit") + &job("test-contract", "test-contract")),
        &(job("test-contract", "test-contract") + &job("test-unit", "test-unit")),
    );
    assert_failure(&makefile, &reordered, "gate order differs");
}
#[test]
fn unsupported_yaml_and_shell_fail_closed() {
    let (makefile, workflow) = fixture();
    for input in [
        workflow.replace("run: make fmt-check", "run: make fmt-check || true"),
        workflow.replace("    name: format", "    if: false\n    name: format"),
        workflow.replace("name: CI", "name: Other"),
        String::new(),
    ] {
        assert!(evaluate(&makefile, &input).is_err());
    }
    assert_failure("ci: $(GATES)", &workflow, "missing gate");
}
#[test]
fn screen_provisioning_requires_exact_steps_and_unconditional_gates() {
    let (makefile, workflow) = fixture();
    for fragment in [
        "            ~/.npm\n",
        "            ~/.cache/ms-playwright\n",
        "          key: screen-${{ runner.os }}-${{ hashFiles('web/screen/package-lock.json') }}\n",
        "      - name: Provision locked screen npm cache\n",
        "        run: npm --prefix web/screen ci --offline=false\n",
        "      - name: Provision pinned screen Chromium\n",
        "        run: cd web/screen && npx --offline playwright install --with-deps chromium\n",
        "      - name: Chromium sandbox user namespaces\n",
        "        uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02\n",
        "          name: screen-evidence-${{ github.sha }}\n",
        "            web/screen/test-results/screenshots/**\n",
        "            web/screen/test-results/summary.txt\n",
        "          if-no-files-found: error\n",
    ] {
        for replacement in [String::new(), format!("{fragment}        if: false\n")] {
            assert!(evaluate(&makefile, &workflow.replace(fragment, &replacement)).is_err());
        }
    }
    for (from, to) in [
        (
            "hashFiles('web/screen/package-lock.json')",
            "hashFiles('web/screen/package.json')",
        ),
        (
            "npm --prefix web/screen ci --offline=false",
            "npm --prefix web/screen install",
        ),
        (
            "playwright install --with-deps chromium",
            "playwright install --with-deps firefox",
        ),
        (
            "run: make test-unit-python test-unit-screen\n",
            "run: make test-unit-python test-unit-screen || true\n",
        ),
    ] {
        assert!(evaluate(&makefile, &workflow.replace(from, to)).is_err());
    }
}
#[test]
fn shared_sim_provisioning_requires_each_exact_host_step() {
    let sim = shared::expected_sim_action();
    let setup = include_str!("../../../.github/actions/setup/action.yml");
    assert_eq!(shared::check_setup(&sim, setup), Ok(()));
    for fragment in [
        "    - name: Host build dependencies\n",
        "      run: sudo apt-get update && sudo apt-get install -y libopus-dev zlib1g-dev\n",
        "    - uses: actions/cache@0057852bfaa89a56745cba8c7296529d2fc39830\n",
        "        path: .cache/hassmic-host\n",
        "        key: hassmic-host-${{ runner.os }}-${{ hashFiles('hassmic.pin') }}\n",
        "    - name: Build pinned hassmic-host\n",
        "      run: make hassmic-host\n",
        "      run: sh scripts/codex-app-server.sh\n",
        "      run: sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0\n",
    ] {
        assert_eq!(sim.matches(fragment).count(), 1, "{fragment}");
        for replacement in [String::new(), format!("{fragment}      if: false\n")] {
            let error = shared::check_setup(&sim.replace(fragment, &replacement), setup).unwrap_err();
            assert!(error.contains("unsupported shared simulation provisioning"), "{error}");
        }
    }
    for (from, to) in [
        ("libopus-dev", "libopus0"),
        ("zlib1g-dev", "zlib1g"),
        ("0057852bfaa89a56745cba8c7296529d2fc39830", "v4"),
        ("path: .cache/hassmic-host", "path: .cache"),
        ("hashFiles('hassmic.pin')", "hashFiles('Cargo.lock')"),
        ("run: make hassmic-host\n", "run: make hassmic-host || true\n"),
    ] {
        assert!(shared::check_setup(&sim.replace(from, to), setup).is_err());
    }
    let caller = "      if: inputs.sim == 'true'\n";
    for changed in [
        setup.replace(caller, ""),
        setup.replace(caller, "      if: true\n"),
        format!("{setup}# inputs.sim\n"),
    ] {
        let error = shared::check_setup(&sim, &changed).unwrap_err();
        assert!(error.contains("unsupported shared simulation caller"), "{error}");
    }
}
