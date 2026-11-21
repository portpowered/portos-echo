use super::Tools;
use crate::{
    bridge_codex::MuseTool,
    bridge_muse::{Access, Backend},
};
use echo_config::BridgeConfig;
use std::sync::Arc;

impl Tools {
    pub(in crate::bridge_voice) fn muse_enabled(&self) -> bool {
        self.muse.is_some() && self.muse_access.as_ref().is_none_or(Access::allows)
    }

    pub(super) fn muse_for_call(&self) -> Option<MuseTool> {
        if !self.muse_enabled() {
            return None;
        }
        Some(MuseTool::new(
            self.muse.clone()?,
            self.trace.clone()?,
            self.muse_access.clone(),
        ))
    }

    pub(in crate::bridge_voice) fn with_muse_access(mut self, access: Option<Access>) -> Self {
        self.muse_access = access;
        self
    }

    pub(in crate::bridge_voice) fn with_muse(mut self, config: &BridgeConfig, local: bool) -> Self {
        self.muse = config
            .muse
            .as_ref()
            .and_then(|muse| Backend::open(muse, local).ok())
            .map(Arc::new);
        self
    }
}
