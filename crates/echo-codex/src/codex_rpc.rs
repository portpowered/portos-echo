//! Bounded request state. The IO owner supplies monotonic elapsed time.
use super::{
    codex_codec::encode,
    codex_protocol::{Message, RequestId},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    time::Duration,
};

/// Diagnostics carry only classifications and numeric error codes, never payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcError {
    InvalidLimits,
    Malformed,
    LineTooLong,
    PendingFull,
    EventsFull,
    Disconnected,
    Deadline,
    UnknownRequest,
    IdExhausted,
    Remote(i64),
    RemoteTransient(i64),
}

impl fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Codex RPC: {self:?}")
    }
}
impl std::error::Error for RpcError {}

/// Independent bounds for frames, admitted client calls, and unsolicited events.
#[derive(Clone, Copy)]
pub struct Limits {
    pub line_bytes: usize,
    pub pending_requests: usize,
    pub notifications: usize,
    pub request_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            line_bytes: 1_048_576,
            pending_requests: 128,
            notifications: 256,
            request_timeout: Duration::from_secs(10),
        }
    }
}

struct Pending {
    deadline: Duration,
    result: Option<Result<Value, RpcError>>,
}

#[derive(Serialize)]
struct Request<'a, T> {
    id: &'a RequestId,
    method: &'a str,
    params: &'a T,
}

#[derive(Serialize)]
struct Reply<'a, T> {
    id: &'a RequestId,
    result: &'a T,
}

/// One process generation; completed replies occupy admission until consumed.
/// Cancellation removes admission; late or unknown replies are ignored.
/// A new generation must use a new state and fresh initialization, with no replay.
pub struct RpcState {
    limits: Limits,
    next_id: i64,
    pending: BTreeMap<RequestId, Pending>,
    server_requests: BTreeSet<RequestId>,
    events: VecDeque<Message>,
    terminal: Option<RpcError>,
}

impl RpcState {
    /// # Errors
    /// All limits and the request timeout must be nonzero.
    pub fn new(limits: Limits) -> Result<Self, RpcError> {
        if limits.line_bytes == 0
            || limits.pending_requests == 0
            || limits.notifications == 0
            || limits.request_timeout.is_zero()
        {
            return Err(RpcError::InvalidLimits);
        }
        Ok(Self {
            limits,
            next_id: 0,
            pending: BTreeMap::new(),
            server_requests: BTreeSet::new(),
            events: VecDeque::new(),
            terminal: None,
        })
    }

    /// Admit a request and return its ID and bounded physical line.
    /// # Errors
    /// Admission and serialization failures leave no pending request.
    pub fn request(
        &mut self,
        method: &str,
        params: &impl Serialize,
        now: Duration,
    ) -> Result<(RequestId, Vec<u8>), RpcError> {
        self.connected()?;
        if self.pending.len() == self.limits.pending_requests {
            return Err(RpcError::PendingFull);
        }
        let next = self.next_id.checked_add(1).ok_or(RpcError::IdExhausted)?;
        let deadline = now
            .checked_add(self.limits.request_timeout)
            .ok_or(RpcError::InvalidLimits)?;
        let id = RequestId::Number(next);
        let frame = encode(
            &Request {
                id: &id,
                method,
                params,
            },
            self.limits.line_bytes,
        )?;
        self.next_id = next;
        self.pending.insert(id.clone(), Pending { deadline, result: None });
        Ok((id, frame))
    }

    /// Correlate an inbound frame. Queue overflow is a fatal connection error.
    /// # Errors
    /// Return terminal connection status or malformed duplicate server IDs.
    pub fn receive(&mut self, message: Message, now: Duration) -> Result<(), RpcError> {
        self.connected()?;
        self.expire(now);
        match message {
            Message::Response { id, result } => self.complete(&id, Ok(result)),
            Message::Failure { id, code, retryable } => self.complete(
                &id,
                Err(if retryable {
                    RpcError::RemoteTransient(code)
                } else {
                    RpcError::Remote(code)
                }),
            ),
            event => {
                if self.events.len() == self.limits.notifications {
                    return self.fail(RpcError::EventsFull);
                }
                if let Message::Request { id, .. } = &event {
                    if self.server_requests.len() == self.limits.pending_requests {
                        return self.fail(RpcError::PendingFull);
                    }
                    if !self.server_requests.insert(id.clone()) {
                        return self.fail(RpcError::Malformed);
                    }
                }
                self.events.push_back(event);
            }
        }
        Ok(())
    }

    fn complete(&mut self, id: &RequestId, result: Result<Value, RpcError>) {
        if let Some(pending) = self.pending.get_mut(id)
            && pending.result.is_none()
        {
            pending.result = Some(result);
        }
    }

    /// Deadline expiration is driven by the IO owner's clock, including idle time.
    pub fn expire(&mut self, now: Duration) {
        for pending in self.pending.values_mut() {
            if pending.result.is_none() && pending.deadline <= now {
                pending.result = Some(Err(RpcError::Deadline));
            }
        }
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        self.pending
            .values()
            .filter(|pending| pending.result.is_none())
            .map(|pending| pending.deadline)
            .min()
    }

    /// Take a reply once ready. None means this admitted call is still pending.
    /// # Errors
    /// An unknown/cancelled/already-consumed ID returns `UnknownRequest`.
    pub fn take_reply(&mut self, id: &RequestId) -> Result<Option<Result<Value, RpcError>>, RpcError> {
        let pending = self.pending.get_mut(id).ok_or(RpcError::UnknownRequest)?;
        let result = pending.result.take();
        if result.is_some() {
            self.pending.remove(id);
        }
        Ok(result)
    }

    /// Cancel admission without replay; the monotonic ID will never be reused.
    pub fn cancel(&mut self, id: &RequestId) -> bool {
        self.pending.remove(id).is_some()
    }

    /// Accepted events remain drainable even after a connection failure.
    pub fn next_event(&mut self) -> Option<Message> {
        self.events.pop_front()
    }

    /// Answer a server request exactly once using its original numeric/string ID.
    /// # Errors
    /// Reject unsolicited IDs, disconnected sessions and oversized results.
    pub fn respond(&mut self, id: &RequestId, result: &impl Serialize) -> Result<Vec<u8>, RpcError> {
        self.connected()?;
        if !self.server_requests.contains(id) {
            return Err(RpcError::UnknownRequest);
        }
        let frame = encode(&Reply { id, result }, self.limits.line_bytes)?;
        self.server_requests.remove(id);
        Ok(frame)
    }

    /// Notifications share the same byte bound as requests and responses.
    /// # Errors
    /// Return a terminal, serialization or line-limit error.
    pub fn notify(&self, method: &str) -> Result<Vec<u8>, RpcError> {
        self.connected()?;
        encode(&json!({"method": method}), self.limits.line_bytes)
    }

    /// Disconnect fails every still-pending call, retaining earlier completions.
    pub fn disconnect(&mut self, reason: RpcError) {
        if self.terminal.is_some() {
            return;
        }
        self.terminal = Some(reason);
        self.server_requests.clear();
        for pending in self.pending.values_mut() {
            if pending.result.is_none() {
                pending.result = Some(Err(reason));
            }
        }
    }

    fn connected(&self) -> Result<(), RpcError> {
        self.terminal.map_or(Ok(()), Err)
    }

    fn fail(&mut self, error: RpcError) -> Result<(), RpcError> {
        self.disconnect(error);
        Err(error)
    }
}

#[cfg(test)]
#[path = "codex_rpc_tests.rs"]
mod tests;
