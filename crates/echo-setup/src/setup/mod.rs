//! Owned local forwards and read-only setup observers. Completion belongs to the command owner,
//! never to an arbitrary screen message or a closed SSE connection.
mod account;
mod confirmation;
mod connection;
mod events;
mod forward;
mod http;
mod prompt;
mod wake;

pub use account::AccountCommand;
pub use confirmation::{ConfirmationEvidence, WakeConfirmation};
pub use connection::InstalledConnection;
pub use forward::ForwardSession;
pub use http::{Health, ScreenEvents, health};
pub use prompt::{Prompt, Provider};
pub use wake::{WakeAttempt, WakeEvidence};

#[cfg(test)]
mod tests;
