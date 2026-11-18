use serde::Deserialize;
use std::collections::{BTreeSet, HashSet};
use std::process::{Command, Output};

const EXCLUDED_ROOT: &str = "factory/";

struct Rule {
    id: &'static str,
    sources: &'static [&'static str],
    except_sources: &'static [&'static str],
    targets: &'static [&'static str],
    workspace_targets: bool,
    except_targets: &'static [&'static str],
    explanation: &'static str,
}

// The entire layering policy: * is a trailing prefix wildcard only.
const RULES: &[Rule] = &[
    Rule {
        id: "R1",
        sources: &["echo-contracts", "echo-config", "echo-screen-contract", "echo-link"],
        except_sources: &[],
        targets: &[
            "tokio",
            "async-std",
            "smol",
            "reqwest",
            "hyper",
            "rustls",
            "webrtc",
            "str0m",
            "cpal",
            "alsa",
        ],
        workspace_targets: true,
        except_targets: &["echo-contracts", "echo-config", "echo-screen-contract", "echo-link"],
        explanation: "contract/config crates allow only contract peers and pure external dependencies",
    },
    Rule {
        id: "R2",
        sources: &["echo-wyoming", "echo-audio", "echo-live"],
        except_sources: &[],
        targets: &["echo-agent-*", "echo-mcp-*"],
        workspace_targets: false,
        except_targets: &[],
        explanation: "transport/audio crates must not depend on agent or MCP crates",
    },
    Rule {
        id: "R3",
        sources: &["echo-agent-*"],
        except_sources: &[],
        targets: &["echo-agent-*"],
        workspace_targets: false,
        except_targets: &["echo-agent-api"],
        explanation: "agent crates depend on each other only through echo-agent-api",
    },
    Rule {
        id: "R4",
        sources: &["*"],
        except_sources: &["echo-mcp-auth"],
        targets: &["oauth2", "openidconnect"],
        workspace_targets: false,
        except_targets: &[],
        explanation: "only echo-mcp-auth may depend on OAuth packages",
    },
];

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    #[serde(deserialize_with = "dependency_kind")]
    kind: Kind,
}

#[derive(PartialEq)]
enum Kind {
    Normal,
    Build,
    Dev,
}

fn dependency_kind<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Kind, D::Error> {
    match Option::<String>::deserialize(decoder)?.as_deref() {
        None => Ok(Kind::Normal),
        Some("build") => Ok(Kind::Build),
        Some("dev") => Ok(Kind::Dev),
        Some(other) => Err(serde::de::Error::custom(format!(
            "unsupported dependency kind: {other}"
        ))),
    }
}

fn matches(patterns: &[&str], name: &str) -> bool {
    patterns.iter().any(|pattern| {
        pattern
            .strip_suffix('*')
            .map_or_else(|| *pattern == name, |prefix| name.starts_with(prefix))
    })
}

fn workspace(metadata: &Metadata) -> Result<Vec<&Package>, String> {
    let mut ids = HashSet::new();
    for package in &metadata.packages {
        if package.id.is_empty() || package.name.is_empty() || !ids.insert(&package.id) {
            return Err("invalid metadata: empty package identity or duplicate package ID".into());
        }
        if package.dependencies.iter().any(|dep| dep.name.is_empty()) {
            return Err("invalid metadata: empty dependency name".into());
        }
    }
    metadata
        .workspace_members
        .iter()
        .map(|id| {
            metadata
                .packages
                .iter()
                .find(|package| &package.id == id)
                .ok_or_else(|| "invalid metadata: workspace member has no package".into())
        })
        .collect()
}

fn evaluate(input: &[u8]) -> Result<Vec<String>, String> {
    let metadata: Metadata =
        serde_json::from_slice(input).map_err(|error| format!("invalid cargo metadata: {error}"))?;
    let roots = workspace(&metadata)?;
    let names: HashSet<_> = roots.iter().map(|package| package.name.as_str()).collect();
    let mut violations = BTreeSet::new();
    for source in &roots {
        for target in source.dependencies.iter().filter(|dep| dep.kind != Kind::Dev) {
            for rule in RULES {
                if matches(rule.sources, &source.name)
                    && !matches(rule.except_sources, &source.name)
                    && (matches(rule.targets, &target.name)
                        || (rule.workspace_targets && names.contains(target.name.as_str())))
                    && !matches(rule.except_targets, &target.name)
                {
                    violations.insert(format!(
                        "{} -> {} / {}: {}",
                        source.name, target.name, rule.id, rule.explanation
                    ));
                }
            }
        }
    }
    Ok(violations.into_iter().collect())
}

fn command_result(result: std::io::Result<Output>) -> Result<Vec<String>, String> {
    let output = result.map_err(|error| format!("cannot run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!("cargo metadata failed ({})", output.status));
    }
    evaluate(&output.stdout)
}

pub(super) fn run() -> Result<(), String> {
    println!("Scope: Cargo workspace dependencies; excluded root: {EXCLUDED_ROOT} (vendored)");
    let violations = command_result(
        Command::new("cargo")
            .args([
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--offline",
                "--locked",
            ])
            .output(),
    )?;
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn graph(source: &str, targets: &[(&str, Value)]) -> Value {
        let deps: Vec<_> = targets
            .iter()
            .map(|(name, kind)| json!({"name": name, "kind": kind}))
            .collect();
        let mut packages = vec![json!({"id": "opaque-root", "name": source, "dependencies": deps})];
        for (index, (name, _)) in targets.iter().enumerate() {
            packages.push(json!({"id": format!("opaque-{index}"), "name": name, "dependencies": []}));
        }
        let ids: Vec<_> = packages.iter().map(|package| package["id"].clone()).collect();
        json!({"packages": packages, "workspace_members": ids})
    }

    fn check(value: &Value) -> Result<Vec<String>, String> {
        evaluate(value.to_string().as_bytes())
    }

    fn diagnostic(source: &str, target: &str, index: usize) -> String {
        let rule = &RULES[index];
        format!("{source} -> {target} / {}: {}", rule.id, rule.explanation)
    }

    #[test]
    fn r1_contract_peers_and_pure_external_only() {
        let mut pure = graph(
            "echo-contracts",
            &[("serde", json!(null)), ("echo-config", json!(null))],
        );
        pure["workspace_members"] = json!(["opaque-root", "opaque-1"]);
        assert_eq!(check(&pure), Ok(vec![]));
        for (source, target) in [("echo-config", "tokio"), ("echo-contracts", "echo-audio")] {
            assert_eq!(
                check(&graph(source, &[(target, json!(null))])),
                Ok(vec![diagnostic(source, target, 0)])
            );
        }
    }

    #[test]
    fn echo_link_rejects_network_runtime_and_implementation_edges() {
        for target in ["reqwest", "tokio", "cpal", "echo-audio", "echo-wyoming"] {
            assert_eq!(
                check(&graph("echo-link", &[(target, json!(null))])),
                Ok(vec![diagnostic("echo-link", target, 0)])
            );
        }
        assert_eq!(check(&graph("echo-link", &[])), Ok(vec![]));
    }

    #[test]
    fn r2_transport_never_calls_agents_or_mcp() {
        assert_eq!(
            check(&graph("echo-audio", &[("echo-contracts", json!(null))])),
            Ok(vec![])
        );
        for source in ["echo-audio", "echo-wyoming", "echo-live"] {
            for target in ["echo-mcp-client", "echo-agent-loop"] {
                assert_eq!(
                    check(&graph(source, &[(target, json!(null))])),
                    Ok(vec![diagnostic(source, target, 1)])
                );
            }
        }
    }

    #[test]
    fn r3_agent_api_is_the_only_agent_dependency() {
        assert_eq!(
            check(&graph("echo-agent-loop", &[("echo-agent-api", json!(null))])),
            Ok(vec![])
        );
        assert_eq!(
            check(&graph("echo-agent-loop", &[("echo-agent-muse", json!(null))])),
            Ok(vec![diagnostic("echo-agent-loop", "echo-agent-muse", 2)])
        );
    }

    #[test]
    fn r4_auth_owns_both_oauth_packages() {
        for target in ["oauth2", "openidconnect"] {
            assert_eq!(check(&graph("echo-mcp-auth", &[(target, json!(null))])), Ok(vec![]));
            assert_eq!(
                check(&graph("echo-bridge", &[(target, json!(null))])),
                Ok(vec![diagnostic("echo-bridge", target, 3)])
            );
        }
    }

    #[test]
    fn normal_and_build_checked_dev_excluded_including_mixed_declarations() {
        for kind in [json!(null), json!("build")] {
            assert_eq!(
                check(&graph("echo-audio", &[("echo-mcp-client", kind)])),
                Ok(vec![diagnostic("echo-audio", "echo-mcp-client", 1)])
            );
        }
        assert_eq!(
            check(&graph("echo-audio", &[("echo-mcp-client", json!("dev"))])),
            Ok(vec![])
        );
        assert_eq!(
            check(&graph(
                "echo-audio",
                &[("echo-mcp-client", json!("dev")), ("echo-mcp-client", json!("build"))]
            )),
            Ok(vec![diagnostic("echo-audio", "echo-mcp-client", 1)])
        );
    }

    #[test]
    fn renamed_optional_target_dependencies_use_canonical_name() {
        let mut value = graph("echo-audio", &[("echo-mcp-client", json!(null))]);
        value["packages"][0]["dependencies"][0]["rename"] = json!("harmless");
        value["packages"][0]["dependencies"][0]["optional"] = json!(true);
        value["packages"][0]["dependencies"][0]["target"] = json!("cfg(unix)");
        assert_eq!(check(&value), Ok(vec![diagnostic("echo-audio", "echo-mcp-client", 1)]));
        value["workspace_members"] = json!(["opaque-0"]);
        assert_eq!(check(&value), Ok(vec![]));
    }

    #[test]
    fn external_bans_apply_without_external_workspace_membership() {
        for (source, target, rule) in [("echo-config", "tokio", 0), ("echo-bridge", "oauth2", 3)] {
            let mut value = graph(source, &[(target, json!(null))]);
            value["workspace_members"] = json!(["opaque-root"]);
            assert_eq!(check(&value), Ok(vec![diagnostic(source, target, rule)]));
        }
    }

    #[test]
    fn invalid_metadata_fails_closed_and_additive_fields_are_accepted() {
        assert!(evaluate(b"{").unwrap_err().starts_with("invalid cargo metadata:"));
        let clean = graph("xtask", &[]);
        for key in ["packages", "workspace_members"] {
            let mut value = clean.clone();
            value.as_object_mut().unwrap().remove(key);
            assert!(check(&value).unwrap_err().contains(key));
        }
        for key in ["id", "name", "dependencies"] {
            let mut value = clean.clone();
            value["packages"][0].as_object_mut().unwrap().remove(key);
            assert!(check(&value).unwrap_err().contains(key));
        }
        let mut value = clean.clone();
        value["workspace_members"] = json!(["missing"]);
        assert_eq!(
            check(&value),
            Err("invalid metadata: workspace member has no package".into())
        );
        value = clean;
        value["future_field"] = json!({"anything": true});
        assert_eq!(check(&value), Ok(vec![]));
    }

    #[test]
    fn missing_or_unknown_dependency_kinds_fail_closed() {
        let mut value = graph("xtask", &[("serde", json!("future"))]);
        assert!(
            check(&value)
                .unwrap_err()
                .contains("unsupported dependency kind: future")
        );
        value["packages"][0]["dependencies"][0]
            .as_object_mut()
            .unwrap()
            .remove("kind");
        assert!(check(&value).unwrap_err().contains("kind"));
        value["packages"][0]["dependencies"][0] = json!({"kind": null});
        assert!(check(&value).unwrap_err().contains("name"));
    }

    #[test]
    fn diagnostics_are_sorted_unique_and_independent_of_package_order() {
        assert_eq!(check(&graph("xtask", &[])), Ok(vec![]));
        let mut value = graph(
            "echo-audio",
            &[
                ("echo-mcp-z", json!(null)),
                ("echo-agent-a", json!(null)),
                ("echo-mcp-z", json!("build")),
            ],
        );
        let expected = Ok(vec![
            diagnostic("echo-audio", "echo-agent-a", 1),
            diagnostic("echo-audio", "echo-mcp-z", 1),
        ]);
        assert_eq!(check(&value), expected);
        value["packages"].as_array_mut().unwrap().reverse();
        assert_eq!(check(&value), expected);
    }

    #[test]
    fn command_spawn_and_exit_errors_never_decode_stdout() {
        assert_eq!(
            command_result(Err(std::io::Error::other("injected spawn failure"))),
            Err("cannot run cargo metadata: injected spawn failure".into())
        );
        let status = failure_status();
        let expected = format!("cargo metadata failed ({status})");
        assert_eq!(
            command_result(Ok(Output {
                status,
                stdout: b"{}".to_vec(),
                stderr: vec![]
            })),
            Err(expected)
        );
    }

    #[cfg(unix)]
    fn failure_status() -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1 << 8)
    }
}
