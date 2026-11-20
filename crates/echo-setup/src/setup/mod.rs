//! Read-only setup observers. Completion still belongs to the command owner,
//! never to an arbitrary screen message or a closed SSE connection.
mod events;
mod http;
mod prompt;

pub use http::{Health, ScreenEvents, health};
pub use prompt::{Prompt, Provider};

#[cfg(test)]
mod tests;
