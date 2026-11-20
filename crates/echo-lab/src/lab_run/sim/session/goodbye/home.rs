//! The agent home of an installed Echo, for the goodbye journeys.
use std::path::Path;

/// An installed Echo: the installer seeded the home's prompt files once from
/// `examples/prompts` and never rewrites them. They replace the built-in base
/// prompts and say nothing about ending a session.
pub(super) fn installed(root: &Path) -> Result<(), String> {
    let home = root.join("agent-home");
    std::fs::create_dir_all(home.join("prompts")).map_err(|_| "goodbye: cannot create the agent home")?;
    for (name, text) in [
        (
            "voice.md",
            include_str!("../../../../../../../examples/prompts/voice.md"),
        ),
        (
            "runner.md",
            include_str!("../../../../../../../examples/prompts/runner.md"),
        ),
    ] {
        if text.contains("end_session") || text.contains("farewell") {
            return Err("goodbye: the seeded prompt files must not carry the goodbye guidance".into());
        }
        std::fs::write(home.join("prompts").join(name), text).map_err(|_| "goodbye: cannot seed a prompt file")?;
    }
    let path = root.join("configured.private/bridge.toml");
    let text = std::fs::read_to_string(&path).map_err(|_| "goodbye: missing configuration")?;
    // Only the prompt files matter here: the notes and prompt tools stay off
    // so the delegated catalog is the session tools alone.
    let table = format!("{text}\n[home]\ndir = '{}'\nagent_tools = false\n", home.display());
    std::fs::write(path, table).map_err(|_| "goodbye: cannot configure the agent home".into())
}
