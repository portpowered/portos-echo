//! M4 owned peers. Component witnesses here do not replace the pinned-host
//! scenario; the production driver will use these same external-boundary fakes.
pub(super) mod codex;
pub(crate) mod emotion;
mod golden;
#[cfg(test)]
mod model;

pub const SUCCESS: &str = "test echo: delegated echo";
pub const FALLBACK: &str = "I couldn't call test echo.";
pub(super) mod screen;
pub(super) mod wire;
pub(super) mod world;

#[cfg(test)]
mod tests;

mod driver;
pub use driver::{M4, emotion_utterance, negative, utterance};
