//! Compare path literals without resolving, opening or inspecting protected state.
use std::path::{Component, Path, PathBuf};

pub(super) fn separate(config: &echo_config::BridgeConfig, nominated: &Path, output: &Path) -> Result<(), String> {
    let nominated = lexical(nominated)?;
    let output = lexical(output)?;
    let protected = config
        .codex
        .home
        .iter()
        .chain(config.mcp.state_dir.iter())
        .chain(std::iter::once(&config.live.credential_file))
        .chain(
            config
                .mcp
                .server
                .iter()
                .filter_map(|server| server.bearer_file.as_ref()),
        );
    for path in protected {
        let path = lexical(path)?;
        if nominated.starts_with(&path) || output.starts_with(&path) {
            return Err("device: configuration and output must be outside protected state".into());
        }
    }
    Ok(())
}

fn lexical(path: &Path) -> Result<PathBuf, String> {
    let absolute = std::env::current_dir()
        .map_err(|_| "device: cannot determine launch directory")?
        .join(path);
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_real_paths_compare_literals_without_creating_absent_state() {
        let mut config =
            echo_config::BridgeConfig::from_toml_str(include_str!("../../../../../../examples/bridge.example.toml"))
                .unwrap();
        config.codex.home = Some("synthetic-absent-home".into());
        config.mcp.state_dir = Some("synthetic-absent-mcp".into());
        for path in [
            "synthetic-absent-home/report",
            "other/../synthetic-absent-home/report",
            "synthetic-absent-mcp/report",
        ] {
            assert_eq!(
                separate(&config, Path::new("bridge.toml"), Path::new(path)),
                Err("device: configuration and output must be outside protected state".into())
            );
        }
        assert_eq!(
            separate(&config, Path::new("bridge.toml"), Path::new("outside/report")),
            Ok(())
        );
    }
}
