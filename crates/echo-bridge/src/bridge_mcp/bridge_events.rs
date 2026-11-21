//! Closed MCP lifecycle observations; no grant, endpoint or response fields.
use super::Grant;
use echo_mcp_auth::AuthError;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

impl Grant {
    pub(super) fn event(&self, name: &'static str, fields: Value) -> Result<(), AuthError> {
        let result = self
            .trace
            .lock()
            .map_err(|_| ())
            .and_then(|mut trace| trace.record(name, None, fields).map_err(|_| ()));
        if result.is_err() {
            self.trace_failed.store(true, Ordering::Release);
            self.rejected.store(true, Ordering::Release);
            return Err(AuthError::Storage);
        }
        Ok(())
    }

    pub(super) fn next_operation(&self) -> Result<u64, AuthError> {
        self.operation
            .try_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
            .map(|id| id + 1)
            .map_err(|_| AuthError::Protocol)
    }

    /// Say which link step failed and how: a fixed category, the step name, the
    /// HTTP status and the OAuth error code. Never a body, id, code or URL.
    pub(super) fn link_failed(&self, id: u64, error: &AuthError) {
        let failure = self.client.take_link_failure();
        let fields = json!({
            "operation_id": id,
            "category": super::category(error),
            "step": failure.as_ref().map(|failure| failure.step),
            "http_status": failure.as_ref().and_then(|failure| failure.http_status),
            "oauth_error": failure.as_ref().and_then(|failure| failure.oauth_error.as_deref()),
        });
        let _ = self.event("mcp.link.failed", fields);
    }

    pub(super) fn used(&self) -> Result<(), AuthError> {
        if self.reloaded.swap(false, Ordering::AcqRel) {
            self.event("mcp.grant.reloaded", json!({}))?;
        }
        self.check().map_err(|_| AuthError::Storage)
    }

    pub(super) fn unauthorized(&self) {
        let first = !self.rejected.swap(true, Ordering::AcqRel);
        self.reject(super::bridge_presentation::RELINK);
        if first {
            let _ = self.event("mcp.relink_required", json!({"reason":"unauthorized"}));
        }
    }
}
