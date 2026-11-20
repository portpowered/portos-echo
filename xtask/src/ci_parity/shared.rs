use std::fs;

use super::shards::{CODEX, HOST, NAMESPACE};

// The sim part jobs provision host and Codex dependencies through the shared sim action, which
// the setup action calls when `sim` is true. Both files must hold exactly the reviewed steps.
pub(super) fn expected_sim_action() -> String {
    let mut shared = String::from(
        "name: Pinned simulation setup\ndescription: Shared host and verified Codex provisioning\nruns:\n  using: composite\n  steps:\n",
    );
    for line in [HOST, CODEX, NAMESPACE].concat().lines() {
        let line = &line[2..];
        if line.starts_with("      run:") {
            shared.push_str("      shell: sh\n");
        }
        shared.push_str(line);
        shared.push('\n');
    }
    shared
}

pub(super) fn check_setup(sim: &str, setup: &str) -> Result<(), String> {
    if sim.replace("\r\n", "\n") != expected_sim_action() {
        return Err("ci-parity: unsupported shared simulation provisioning".into());
    }
    let setup = setup.replace("\r\n", "\n");
    let input = "  sim:\n    description: Provision pinned simulation dependencies\n    default: 'false'\nruns:\n";
    let call =
        "    - name: Simulation dependencies\n      if: inputs.sim == 'true'\n      uses: ./.github/actions/sim\n";
    if !setup.contains(input) || !setup.ends_with(call) || setup.matches("inputs.sim").count() != 1 {
        return Err("ci-parity: unsupported shared simulation caller".into());
    }
    Ok(())
}

pub(super) fn check() -> Result<(), String> {
    let sim = fs::read_to_string(".github/actions/sim/action.yml").map_err(|e| e.to_string())?;
    let setup = fs::read_to_string(".github/actions/setup/action.yml").map_err(|e| e.to_string())?;
    check_setup(&sim, &setup)
}
