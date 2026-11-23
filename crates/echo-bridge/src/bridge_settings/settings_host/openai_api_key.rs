//! The `api_key`-mode credential, replaced from Settings. The key stays in
//! `Secret`; snapshots report only whether a well-formed key file exists.
use super::{SettingsError, SettingsHost};
use crate::bridge_settings::settings_atomic;
use echo_config::LiveMode;
use echo_contracts::secret::Secret;
use echo_screen_contract::settings_v2::{OpenAiApiKeyRequestV2, OpenAiApiKeyStatusV2};
use std::{io::Read, path::Path};

const INVALID: SettingsError = SettingsError::Prompt("openai.api_key: expected one line of printable text");
const CHATGPT: SettingsError = SettingsError::Prompt("openai.api_key: this Echo uses ChatGPT sign-in");
const LIMIT: u64 = 16_384;

impl SettingsHost {
    /// `None` in `chatgpt` mode, so the screen keeps its sign-in controls.
    pub(crate) fn api_key(&self) -> Option<OpenAiApiKeyStatusV2> {
        let live = &self.store.effective().live;
        (live.mode == LiveMode::ApiKey).then(|| OpenAiApiKeyStatusV2 {
            configured: stored(&live.credential_file).is_some(),
        })
    }

    /// Keep the replaced key beside the new one as `<name>.previous`, then
    /// request the settled restart that logs the app server in again. `OpenAI`
    /// first checks the key at the next call, so it cannot be tested here.
    pub(super) fn save_openai_api_key(&mut self, request: OpenAiApiKeyRequestV2) -> Result<bool, SettingsError> {
        let file = self.store.effective().live.credential_file.clone();
        if self.store.effective().live.mode != LiveMode::ApiKey {
            return Err(CHATGPT);
        }
        let key = echo_signin::api_key(request.api_key.into_inner()).map_err(|_| INVALID)?;
        // The per-call reader refuses a longer file, and a pasted space is never part of a key.
        let text = key.expose();
        if u64::try_from(text.len()).map_or(true, |length| length > LIMIT) || text.trim() != text {
            return Err(INVALID);
        }
        if self.store.uncertain {
            return Err(SettingsError::Durability);
        }
        if !self.store.restart_allowed {
            return Err(SettingsError::RestartUnavailable);
        }
        let directory = file.parent().ok_or(SettingsError::Persistence)?;
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(SettingsError::Persistence)?;
        if let Some(previous) = stored(&file) {
            settings_atomic::replace(directory, &format!("{name}.previous"), previous.expose(), |_| Ok(()))?;
        }
        settings_atomic::replace(directory, name, key.expose().as_bytes(), |_| Ok(()))?;
        self.restart_pending = true;
        Ok(true)
    }
}

/// The current file's bytes when it holds a key the bridge would load.
fn stored(path: &Path) -> Option<Secret<Vec<u8>>> {
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|file| file.take(LIMIT + 1).read_to_end(&mut bytes));
    let bytes = Secret::new(bytes);
    read.ok()?;
    let text = Secret::new(std::str::from_utf8(bytes.expose()).ok()?.to_owned());
    let valid = u64::try_from(bytes.expose().len()).is_ok_and(|length| length <= LIMIT)
        && echo_signin::api_key(text.expose().clone()).is_ok();
    valid.then_some(bytes)
}

#[cfg(test)]
#[path = "openai_api_key_tests.rs"]
mod tests;
