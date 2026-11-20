use super::{DispatchError, valid_id};
use echo_agent_api::ToolSpec;
use echo_codex::DynamicTool;
use std::collections::{BTreeMap, HashSet};

/// A deterministic snapshot of the discovered, allowlisted tools. Catalog order
/// is retained; exact aliases resolve only to admitted original MCP names.
pub struct Catalog {
    names: Vec<String>,
    tools: Vec<DynamicTool>,
}

impl Catalog {
    /// Append bridge-local tools with exact names, reserving their names and
    /// aliases even when disabled. Discovery never grants local authority.
    /// # Errors
    /// A discovered MCP name or alias shadows a reserved local tool.
    pub fn with_local(
        mut specs: Vec<ToolSpec>,
        allowed: &[String],
        local: Vec<ToolSpec>,
        reserved: &[&str],
    ) -> Result<Self, DispatchError> {
        let discovered: Vec<_> = specs.iter().map(|spec| spec.name.clone()).collect();
        let mapped = aliases(&discovered);
        let local_names: Vec<_> = local.iter().map(|spec| spec.name.clone()).collect();
        if specs.iter().any(|spec| {
            reserved.contains(&spec.name.as_str())
                || local_names.contains(&spec.name)
                || reserved.contains(&base(&spec.name).as_str())
                || (allowed.contains(&spec.name) && local_names.contains(&base(&spec.name)))
        }) || mapped.values().any(|name| reserved.contains(&name.as_str()))
        {
            return Err(DispatchError::Catalog);
        }
        let mut allowed = allowed.to_vec();
        allowed.extend(local_names.iter().cloned());
        specs.extend(local);
        let catalog = Self::new(specs, &allowed)?;
        if local_names
            .iter()
            .any(|name| catalog.resolve(name) != Some(name.as_str()))
        {
            return Err(DispatchError::Catalog);
        }
        Ok(catalog)
    }

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
                name: String::new(),
                description: spec.description,
                input_schema: spec.input_schema,
            });
            names.push(spec.name);
        }
        let aliases = aliases(&names);
        for (tool, name) in tools.iter_mut().zip(&names) {
            tool.name.clone_from(&aliases[name]);
        }
        Ok(Self { names, tools })
    }

    #[must_use]
    pub fn tools(&self) -> &[DynamicTool] {
        &self.tools
    }

    #[must_use]
    pub fn resolve(&self, alias: &str) -> Option<&str> {
        self.tools
            .iter()
            .zip(&self.names)
            .find(|(tool, _)| tool.name == alias)
            .map(|(_, name)| name.as_str())
    }
}

// Reserve every natural base before suffixing collision groups. Sorted groups
// and originals make the mapping independent of MCP discovery order.
fn aliases(names: &[String]) -> BTreeMap<String, String> {
    let mut groups: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for name in names {
        groups.entry(base(name)).or_default().push(name);
    }
    let mut reserved: HashSet<String> = groups.keys().cloned().collect();
    let mut aliases = BTreeMap::new();
    for (base, mut originals) in groups {
        originals.sort();
        let mut ordinal = 0;
        for original in &originals {
            let mut alias = base.clone();
            if originals.len() > 1 {
                loop {
                    ordinal += 1;
                    let suffix = format!("__c{ordinal}");
                    alias = format!("{}{}", &base[..base.len().min(128 - suffix.len())], suffix);
                    if reserved.insert(alias.clone()) {
                        break;
                    }
                }
            }
            aliases.insert((*original).clone(), alias);
        }
    }
    aliases
}

fn base(name: &str) -> String {
    let mut base: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if base.starts_with(|c: char| c.is_ascii_digit()) || base == "mcp" || base.starts_with("mcp__") || reserved(&base) {
        base.insert_str(0, "bridge_");
    }
    base.truncate(128);
    base
}

fn reserved(name: &str) -> bool {
    matches!(
        name,
        "__proto__"
            | "constructor"
            | "prototype"
            | "toString"
            | "valueOf"
            | "hasOwnProperty"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "toLocaleString"
            | "__defineGetter__"
            | "__defineSetter__"
            | "__lookupGetter__"
            | "__lookupSetter__"
            | "exec"
            | "wait"
            | "shell"
            | "shell_command"
            | "unified_exec"
            | "write_stdin"
            | "apply_patch"
            | "view_image"
            | "update_plan"
            | "request_user_input"
            | "request_user_input_async"
            | "send_message_to_user_async"
            | "spawn_agent"
            | "send_message"
            | "wait_agent"
            | "close_agent"
            | "resume_agent"
            | "web_search"
            | "clock__curr_time"
            | "clock__sleep"
    )
}

#[cfg(test)]
mod local_tests {
    use super::*;
    use serde_json::json;

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: "fixture".into(),
            input_schema: json!({"type":"object"}),
        }
    }

    #[test]
    fn web_search_catalog_alias_cannot_shadow_hosted_tool() {
        let catalog = Catalog::new(
            vec![spec("web_search"), spec("test.echo")],
            &["web_search".into(), "test.echo".into()],
        )
        .unwrap();
        assert_eq!(
            catalog
                .tools()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["bridge_web_search", "test_echo"]
        );
        assert_eq!(catalog.resolve("web_search"), None);
        assert_eq!(catalog.resolve("bridge_web_search"), Some("web_search"));
        assert_eq!(catalog.resolve("test_echo"), Some("test.echo"));
    }

    #[test]
    fn agent_tools_catalog_reserves_disabled_local_names_and_aliases() {
        let reserved = ["notes_write", "notes_read", "notes_list", "prompt_read", "prompt_write"];
        for name in [
            "notes_write",
            "notes.write",
            "notes-read",
            "notes/list",
            "prompt.read",
            "prompt_write",
        ] {
            for enabled in [false, true] {
                let local = if enabled { vec![spec("notes_write")] } else { vec![] };
                assert_eq!(
                    Catalog::with_local(vec![spec(name)], &[], local, &reserved).err(),
                    Some(DispatchError::Catalog)
                );
            }
        }
        let catalog = Catalog::with_local(
            vec![spec("test.echo")],
            &["test.echo".into()],
            vec![spec("notes_write")],
            &reserved,
        )
        .unwrap();
        assert_eq!(
            catalog
                .tools()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["test_echo", "notes_write"]
        );
        assert_eq!(catalog.resolve("test_echo"), Some("test.echo"));
        assert_eq!(catalog.resolve("notes_write"), Some("notes_write"));
        assert_eq!(catalog.resolve("prompt_write"), None);
        assert_eq!(catalog.tools()[1].input_schema, json!({"type":"object"}));
    }
}
