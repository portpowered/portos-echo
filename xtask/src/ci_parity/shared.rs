use std::fs;

// Expand only the reviewed caller after validating the extracted implementation.
// The existing canonical grammar and all its self-tests remain unchanged.
pub(super) fn expand(workflow: &str) -> Result<String, String> {
    let canonical = super::job("test-sim", "test-sim");
    let start = canonical
        .find("      - name: Host build dependencies")
        .ok_or("missing host grammar")?;
    let end = canonical
        .find("      - name: test-sim\n")
        .ok_or("missing sim grammar")?;
    let provision = &canonical[start..end];
    let mut shared = String::from(
        "name: Pinned simulation setup\ndescription: Shared host and verified Codex provisioning\nruns:\n  using: composite\n  steps:\n",
    );
    for line in provision.lines() {
        let line = &line[2..];
        if line.starts_with("      run:") {
            shared.push_str("      shell: sh\n");
        }
        shared.push_str(line);
        shared.push('\n');
    }
    let actual = fs::read_to_string(".github/actions/sim/action.yml").map_err(|e| e.to_string())?;
    if actual.replace("\r\n", "\n") != shared {
        return Err("ci-parity: unsupported shared simulation provisioning".into());
    }
    let setup = fs::read_to_string(".github/actions/setup/action.yml").map_err(|e| e.to_string())?;
    let setup = setup.replace("\r\n", "\n");
    let input = "  sim:\n    description: Provision pinned simulation dependencies\n    default: 'false'\nruns:\n";
    let call =
        "    - name: Simulation dependencies\n      if: inputs.sim == 'true'\n      uses: ./.github/actions/sim\n";
    if !setup.contains(input) || !setup.ends_with(call) || setup.matches("inputs.sim").count() != 1 {
        return Err("ci-parity: unsupported shared simulation caller".into());
    }
    let factored = canonical.replace(provision, "").replace(
        "          target: test-sim\n",
        "          target: test-sim\n          sim: 'true'\n",
    );
    if workflow.matches(&factored).count() != 1 {
        return Err("ci-parity: unsupported job test-sim; expected exact shared setup caller".into());
    }
    Ok(workflow.replace(&factored, &canonical))
}
