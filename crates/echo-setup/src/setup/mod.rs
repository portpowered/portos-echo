//! Owned local forwards and read-only setup observers. Completion belongs to the command owner,
//! never to an arbitrary screen message or a closed SSE connection.
mod confirmation;
mod events;
mod forward;
mod http;
mod prompt;
mod wake;

pub use confirmation::{ConfirmationEvidence, WakeConfirmation};
pub use forward::ForwardSession;
pub use http::{Health, ScreenEvents, health};
pub use prompt::{Prompt, Provider};
pub use wake::{WakeAttempt, WakeEvidence};

#[cfg(test)]
mod tests;
