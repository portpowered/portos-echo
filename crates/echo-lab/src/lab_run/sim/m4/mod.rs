//! M4 owned peers. Component witnesses here do not replace the pinned-host
//! scenario; the production driver will use these same external-boundary fakes.
mod emotion;
mod golden;
mod model;
mod screen;
mod wire;
mod world;

#[cfg(test)]
mod tests;

mod driver;
pub use driver::{M4, emotion_utterance, negative, utterance};
