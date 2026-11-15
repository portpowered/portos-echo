use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! correlation_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(
            #[serde(deserialize_with = "non_empty")]
            #[schemars(length(min = 1))]
            String,
        );
        impl $name {
            /// Construct an identifier without trimming or rewriting its value.
            /// # Errors
            /// Returns `InvalidId` only for an empty string.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidId> {
                let value = value.into();
                if value.is_empty() {
                    return Err(InvalidId);
                }
                Ok(Self(value))
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}
correlation_id!(SessionId);
correlation_id!(CallId);
correlation_id!(TurnId);
correlation_id!(DelegationId);
correlation_id!(TaskId);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("correlation id must not be empty")]
pub struct InvalidId;

fn non_empty<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        return Err(serde::de::Error::custom(InvalidId));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    macro_rules! check {
        ($name:ident) => {
            for text in ["s-1", "\u{6c34}", " "] {
                let id = $name::new(text).unwrap();
                assert_eq!(id.to_string(), text);
                let json = serde_json::to_string(text).unwrap();
                assert_eq!(serde_json::to_string(&id).unwrap(), json);
                assert_eq!(serde_json::from_str::<$name>(&json).unwrap(), id);
            }
            assert_eq!($name::new(""), Err(InvalidId));
            assert!(
                serde_json::from_str::<$name>("\"\"")
                    .unwrap_err()
                    .to_string()
                    .contains("must not be empty")
            );
        };
    }
    #[test]
    fn sessionid_preserves_exact_values() {
        check!(SessionId);
    }
    #[test]
    fn callid_preserves_exact_values() {
        check!(CallId);
    }
    #[test]
    fn turnid_preserves_exact_values() {
        check!(TurnId);
    }
    #[test]
    fn delegationid_preserves_exact_values() {
        check!(DelegationId);
    }
    #[test]
    fn taskid_preserves_exact_values() {
        check!(TaskId);
    }
}
