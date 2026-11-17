use super::{DispatchError, valid_id};
use echo_agent_api::ToolSpec;
use echo_codex::DynamicTool;
use std::collections::HashSet;

/// A deterministic snapshot of the discovered, allowlisted tools. Catalog order
/// is retained; names are never supplied directly to the model as executables.
pub struct Catalog {
    names: Vec<String>,
    tools: Vec<DynamicTool>,
}

impl Catalog {
    /// # Errors
    /// Oversized/ambiguous catalogs and malformed advertised tool definitions.
    pub fn new(specs: Vec<ToolSpec>, allowed: &[String]) -> Result<Self, DispatchError> {
        if specs.len() > 1024 {
            return Err(DispatchError::Catalog);
        }
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        let mut tools = Vec::new();
        for spec in specs {
            if !valid_id(&spec.name) || !seen.insert(spec.name.clone()) || !spec.input_schema.is_object() {
                return Err(DispatchError::Catalog);
            }
            if !allowed.contains(&spec.name) {
                continue;
            }
            tools.push(DynamicTool {
                name: format!("tool_{}", names.len()),
                description: spec.description,
                input_schema: spec.input_schema,
            });
            names.push(spec.name);
        }
        Ok(Self { names, tools })
    }

    #[must_use]
    pub fn tools(&self) -> &[DynamicTool] {
        &self.tools
    }

    #[must_use]
    pub fn resolve(&self, alias: &str) -> Option<&str> {
        let index = alias.strip_prefix("tool_")?.parse::<usize>().ok()?;
        if alias != format!("tool_{index}") {
            return None;
        }
        self.names.get(index).map(String::as_str)
    }
}
