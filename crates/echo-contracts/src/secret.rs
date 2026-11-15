use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }
    pub fn expose(&self) -> &T {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn transparent_deserialization_and_borrow() {
        let secret: Secret<String> = serde_json::from_str("\"synthetic-value\"").unwrap();
        assert_eq!(secret.expose(), "synthetic-value");
        assert!(std::ptr::eq(secret.expose(), secret.expose()));
    }
    struct CountDrop<'a>(&'a Cell<usize>);
    impl Drop for CountDrop<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    #[test]
    fn ordinary_wrapped_value_drops_once() {
        let count = Cell::new(0);
        let secret = Secret::new(CountDrop(&count));
        assert_eq!(count.get(), 0);
        drop(secret);
        assert_eq!(count.get(), 1);
    }
}
