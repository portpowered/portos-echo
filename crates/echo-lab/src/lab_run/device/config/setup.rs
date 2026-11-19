//! Offline setup admission. Only the nominated TOML is inspected.
use super::super::super::run_options::Options;
use echo_config::{BridgeConfig, McpAuth};
use serde_json::Value;

pub fn validate(scenario: &Value) -> Result<(), String> {
    for (name, value) in scenario["setup"].as_object().ok_or("device: missing setup")? {
        if !(matches!(
            (name.as_str(), value.as_str()),
            ("mcp", Some("spoof")) | ("backend", Some("client"))
        ) || scenario["id"] == "m5-port-os-link" && name == "mcp" && value == "portos-pilot")
        {
            return Err(format!("device: unsupported setup {name}"));
        }
    }
    Ok(())
}

pub fn requested(options: &Options) -> Result<bool, String> {
    let mut spoof = false;
    for path in &options.scenarios {
        let scenario = crate::lab_validate::load(path)?;
        validate(&scenario)?;
        spoof |= scenario["setup"]["mcp"] == "spoof";
    }
    Ok(spoof)
}

pub fn port(config: &BridgeConfig) -> Result<u16, String> {
    let error = "device: unmet prerequisite: one unauthenticated numeric-loopback spoof MCP endpoint for test.echo";
    let mut servers = config
        .mcp
        .server
        .iter()
        .filter(|server| server.allow_tools.iter().any(|tool| tool == "test.echo"));
    let server = servers.next().ok_or(error)?;
    if servers.next().is_some() || server.auth != McpAuth::None || server.bearer_file.is_some() {
        return Err(error.into());
    }
    let url = url::Url::parse(&server.url).map_err(|_| error)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/mcp"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(error.into());
    }
    let port = url.port().filter(|port| *port != 0).ok_or(error)?;
    // URL canonicalization must not turn an alternative host spelling into
    // authority to bind it. Never log the nominated URL or other MCP entries.
    if server.url != format!("http://127.0.0.1:{port}/mcp") {
        return Err(error.into());
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BridgeConfig {
        BridgeConfig::from_toml_str(include_str!("../../../../../../examples/bridge.example.toml")).unwrap()
    }

    #[test]
    fn device_setup_requires_one_exact_spoof_endpoint_without_reading_other_servers() {
        let mut value = config();
        let original = value.mcp.server[0].clone();
        assert_eq!(port(&value), Ok(18080));
        let mut unrelated = original.clone();
        unrelated.name = "unrelated".into();
        unrelated.url = "https://service.example.invalid/mcp".into(); // lint-rules: allow-literal inert unrelated config entry, never connected
        unrelated.auth = McpAuth::BearerFile;
        unrelated.bearer_file = Some("absent-protected-state".into());
        unrelated.allow_tools = vec!["another.tool".into()];
        value.mcp.server.push(unrelated);
        let before = value.clone();
        assert_eq!(port(&value), Ok(18080));
        assert_eq!(value, before);
        value.mcp.server.push(original);
        assert!(port(&value).unwrap_err().contains("one unauthenticated"));
    }

    #[test]
    fn device_setup_rejects_ambiguous_or_credentialed_spoof_nominations() {
        for url in [
            "http://localhost:18080/mcp",
            "https://127.0.0.1:18080/mcp",
            "http://127.0.0.1:18080/mcp?extra",
            "http://127.0.0.1:18080/mcp#extra",
            "http://127.0.0.1:18080/other",
            "http://127.0.0.1:0/mcp",
            "http://127.1:18080/mcp", // lint-rules: allow-literal offline noncanonical URL refusal, never connected
            "http://127.0.0.1/mcp",
        ] {
            let mut value = config();
            value.mcp.server[0].url = url.into();
            assert!(port(&value).unwrap_err().contains("numeric-loopback"), "{url}");
        }
        let mut value = config();
        value.mcp.server[0].auth = McpAuth::BearerFile;
        assert!(port(&value).is_err());
        value.mcp.server[0].auth = McpAuth::None;
        value.mcp.server[0].bearer_file = Some("absent-protected-state".into());
        assert!(port(&value).is_err());
        value.mcp.server.clear();
        assert!(port(&value).is_err());
    }
}
