//! Owner skills are admitted once and read from the same snapshot for every call.
use crate::{Error, bridge_notes::notes_files as files};
use echo_agent_api::{ToolResult, ToolSpec};
use echo_codex::codex_rpc::Limits;
use echo_config::BridgeConfig;
use serde_json::{Value, json};
use std::{borrow::Cow, ffi::OsStr, fmt::Write as _, sync::Arc};

pub(super) const READ_SKILL: &str = "read_skill";

#[derive(Clone, Default)]
pub struct RunnerSkills(Arc<[Skill]>);

struct Skill {
    name: String,
    description: String,
    body: String,
}

impl RunnerSkills {
    pub(crate) fn load(config: &BridgeConfig) -> Result<Self, Error> {
        config
            .validate_skill_selection()
            .map_err(|error| Error::PromptConfig(error.to_string()))?;
        if config.codex.skills.is_empty() {
            return Ok(Self::default());
        }
        let home = config.home.as_ref().ok_or(Error::Input)?;
        let root = files::root_named(&home.dir, "skills", false).map_err(|reason| skill_error(0, reason))?;
        let skills = config
            .codex
            .skills
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let root = root.as_ref().ok_or_else(|| skill_error(index, "unknown skill"))?;
                Skill::load(root, name).map_err(|reason| skill_error(index, reason))
            })
            .collect::<Result<Arc<[_]>, _>>()?;
        Ok(Self(skills))
    }

    pub(in crate::bridge_voice) fn append_instructions(&self, prompt: &mut String) {
        if self.0.is_empty() {
            return;
        }
        prompt.push_str("\n\nEnabled owner skills (use read_skill by name to load the body):");
        for skill in self.0.iter() {
            let _ = write!(prompt, "\n- {} — {}", skill.name, skill.description);
        }
    }

    pub(super) fn spec(&self) -> Option<ToolSpec> {
        (!self.0.is_empty()).then(|| ToolSpec {
            name: READ_SKILL.into(),
            description: "Read the Markdown body of an enabled owner skill.".into(),
            input_schema: json!({"type":"object","properties":{"name":{"type":"string"}},
                "required":["name"],"additionalProperties":false}),
        })
    }

    pub(super) fn read(&self, args: &Value) -> ToolResult {
        let result = args
            .as_object()
            .filter(|object| object.len() == 1)
            .and_then(|object| object.get("name"))
            .and_then(Value::as_str)
            .ok_or("invalid_arguments")
            .and_then(|name| {
                self.0
                    .iter()
                    .find(|skill| skill.name == name)
                    .ok_or("skill_not_enabled")
            });
        ToolResult {
            is_error: result.is_err(),
            content: match result {
                Ok(skill) => json!({"body":skill.body}),
                Err(code) => json!({"error":code}),
            },
        }
    }
}

impl Skill {
    fn load(root: &impl rustix::fd::AsFd, name: &str) -> Result<Self, &'static str> {
        let dir = files::directory(root, OsStr::new(name), false)?.ok_or("unknown skill")?;
        let limit = Limits::default().line_bytes;
        let bytes = files::read_limited(&dir, "SKILL.md", limit as u64).map_err(|reason| match reason {
            "not_found" => "unknown skill",
            "note_too_large" => "skill exceeds the Codex message limit",
            other => other,
        })?;
        let text = std::str::from_utf8(&bytes).map_err(|_| "expected UTF-8 SKILL.md")?;
        let (actual, description, body) = front_matter(text).ok_or("invalid SKILL.md front matter")?;
        if actual != name {
            return Err("SKILL.md name does not match the configured skill");
        }
        // The body is encoded inside a JSON text content item. Leave room for
        // the surrounding RPC response and its correlation ID.
        if json!(json!({"body":body}).to_string()).to_string().len() + 1024 > limit {
            return Err("skill exceeds the Codex message limit");
        }
        Ok(Self {
            name: name.into(),
            description: description.into_owned(),
            body: body.into(),
        })
    }
}

fn skill_error(index: usize, reason: &str) -> Error {
    Error::PromptConfig(format!("codex.skills[{index}]: {reason}"))
}

fn front_matter(text: &str) -> Option<(Cow<'_, str>, Cow<'_, str>, &str)> {
    let mut lines = text.split_inclusive('\n');
    let opening = lines.next()?;
    if opening.trim_end_matches(['\r', '\n']) != "---" {
        return None;
    }
    let mut offset = opening.len();
    let (mut name, mut description) = (None, None);
    for line in lines {
        offset += line.len();
        let line = line.trim_end_matches(['\r', '\n']);
        if line == "---" {
            return Some((name?, description?, &text[offset..]));
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let field = match key {
            "name" => &mut name,
            "description" => &mut description,
            _ => continue,
        };
        if field.is_some() {
            return None;
        }
        *field = Some(scalar(value.trim())?);
    }
    None
}

fn scalar(value: &str) -> Option<Cow<'_, str>> {
    let scalar = match value.as_bytes().first()? {
        b'"' => Cow::Owned(serde_json::from_str::<String>(value).ok()?),
        b'\'' => Cow::Owned(value.strip_prefix('\'')?.strip_suffix('\'')?.replace("''", "'")),
        b'|' | b'>' | b'#' => return None,
        _ => Cow::Borrowed(value.split_once(" #").map_or(value, |(text, _)| text.trim_end())),
    };
    (!scalar.trim().is_empty() && !scalar.contains(['\u{2028}', '\u{2029}']) && !scalar.chars().any(char::is_control))
        .then_some(scalar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink, path::Path};

    fn config(home: &Path, names: &[&str]) -> BridgeConfig {
        let mut config =
            BridgeConfig::from_toml_str(include_str!("../../../../../../examples/bridge.echo.toml")).unwrap();
        config.home.as_mut().unwrap().dir = home.into();
        config.codex.skills = names.iter().map(|name| (*name).into()).collect();
        config
    }

    #[test]
    fn runner_skills_never_follows_skill_directory_or_file_symlinks() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = home.path().join("skills");
        fs::write(
            outside.path().join("SKILL.md"),
            "---\nname: brief\ndescription: Outside\n---\nOutside.",
        )
        .unwrap();
        let config = config(home.path(), &["brief"]);
        let assert_refused = || {
            assert_eq!(
                RunnerSkills::load(&config).err(),
                Some(Error::PromptConfig("codex.skills[0]: symlink".into()))
            );
        };
        symlink(outside.path(), &root).unwrap();
        assert_refused();
        fs::remove_file(&root).unwrap();
        fs::create_dir(&root).unwrap();
        let skill = root.join("brief");
        symlink(outside.path(), &skill).unwrap();
        assert_refused();
        fs::remove_file(&skill).unwrap();
        fs::create_dir(&skill).unwrap();
        symlink(outside.path().join("SKILL.md"), skill.join("SKILL.md")).unwrap();
        assert_refused();
    }

    #[test]
    fn runner_skills_admits_only_selected_metadata_and_keeps_exact_body_after_edits() {
        let home = tempfile::tempdir().unwrap();
        let config = config(home.path(), &["brief"]);
        let active = home.path().join("skills/brief");
        fs::create_dir_all(&active).unwrap();
        let file = active.join("SKILL.md");
        let body = "\r\nSummarize.  \r\n---\r\nFinal line";
        fs::write(
            &file,
            format!("---\r\nname: brief\r\ndescription: \"Avoid a long answer.\"\r\n---\r\n{body}"),
        )
        .unwrap();
        let admitted = RunnerSkills::load(&config).unwrap();
        fs::write(&file, "changed after admission").unwrap();
        let mut prompt = String::from("runner and call context");
        admitted.append_instructions(&mut prompt);
        assert_eq!(
            prompt,
            "runner and call context\n\nEnabled owner skills (use read_skill by name to load the body):\n- brief — Avoid a long answer."
        );
        let result = admitted.read(&json!({"name":"brief"}));
        assert!(!result.is_error);
        assert_eq!(result.content, json!({"body":body}));
        for (args, error) in [
            (json!({"name":"unused"}), "skill_not_enabled"),
            (json!({"name":"../brief"}), "skill_not_enabled"),
            (json!({"name":"brief","extra":true}), "invalid_arguments"),
            (json!({"name":1}), "invalid_arguments"),
        ] {
            let result = admitted.read(&args);
            assert!(result.is_error);
            assert_eq!(result.content, json!({"error":error}));
        }
    }

    #[test]
    fn runner_skills_reports_missing_selection_at_its_index_and_ignores_unselected_files() {
        let home = tempfile::tempdir().unwrap();
        let mut config = config(home.path(), &[]);
        let selected = home.path().join("skills/brief");
        fs::create_dir_all(&selected).unwrap();
        let file = selected.join("SKILL.md");
        fs::write(&file, "unparseable and inactive").unwrap();
        assert!(RunnerSkills::load(&config).unwrap().spec().is_none());
        config.codex.skills = vec!["brief".into(), "missing".into()];
        fs::write(file, "---\nname: brief\ndescription: Brief.\n---\nBody.\n").unwrap();
        assert_eq!(
            RunnerSkills::load(&config).err(),
            Some(Error::PromptConfig("codex.skills[1]: unknown skill".into()))
        );
    }
}
