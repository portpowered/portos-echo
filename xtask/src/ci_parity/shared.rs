use std::fs;

// The two sim halves call the shared setup; every other job keeps its inline grammar.
const SIM_JOBS: [&str; 2] = ["test-sim-suites", "test-sim-host"];

// Expand only the reviewed callers after validating the extracted implementation.
// The existing canonical grammar and all its self-tests remain unchanged.
pub(super) fn expand(workflow: &str) -> Result<String, String> {
    let provision = |canonical: &str, name: &str| -> Result<(usize, usize), String> {
        let start = canonical
            .find("      - name: Host build dependencies")
            .ok_or("missing host grammar")?;
        let end = canonical
            .find(&format!("      - name: {name}\n"))
            .ok_or("missing sim grammar")?;
        Ok((start, end))
    };
    let first = super::job(SIM_JOBS[0], SIM_JOBS[0]);
    let (start, end) = provision(&first, SIM_JOBS[0])?;
    let mut shared = String::from(
        "name: Pinned simulation setup\ndescription: Shared host and verified Codex provisioning\nruns:\n  using: composite\n  steps:\n",
    );
    for line in first[start..end].lines() {
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
    let mut workflow = workflow.to_string();
    for name in SIM_JOBS {
        let canonical = super::job(name, name);
        let (start, end) = provision(&canonical, name)?;
        let target = format!("          target: {name}\n");
        let factored = canonical
            .replace(&canonical[start..end], "")
            .replace(&target, &format!("{target}          sim: 'true'\n"));
        if workflow.matches(&factored).count() != 1 {
            return Err(format!("ci-parity: unsupported job {name}; expected exact shared setup caller"));
        }
        workflow = workflow.replace(&factored, &canonical);
    }
    Ok(workflow)
}
