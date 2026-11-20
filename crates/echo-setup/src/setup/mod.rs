//! Owned local forwards and read-only setup observers. Completion belongs to the command owner,
//! never to an arbitrary screen message or a closed SSE connection.
mod events;
mod forward;
mod http;
mod prompt;

pub use forward::ForwardSession;
pub use http::{Health, ScreenEvents, health};
pub use prompt::{Prompt, Provider};

#[cfg(test)]
mod tests;
