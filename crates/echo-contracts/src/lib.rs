//! Pure shared bridge vocabulary. No transport or execution policy.
pub mod ids;
pub mod live;
pub mod secret;
pub mod task {
    use crate::ids::TaskId;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum TaskEvent {
        Progress {
            task_id: TaskId,
            message: String,
        },
        Completed {
            task_id: TaskId,
            summary: String,
        },
        Failed {
            task_id: TaskId,
            retryable: bool,
            message: String,
        },
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        #[test]
        fn task_wire_values_and_invalid_nested_id() {
            let id = TaskId::new("task-1").unwrap();
            let events = [
                (
                    TaskEvent::Progress {
                        task_id: id.clone(),
                        message: "working".into(),
                    },
                    json!({"type":"progress","task_id":"task-1","message":"working"}),
                ),
                (
                    TaskEvent::Completed {
                        task_id: id.clone(),
                        summary: "done".into(),
                    },
                    json!({"type":"completed","task_id":"task-1","summary":"done"}),
                ),
                (
                    TaskEvent::Failed {
                        task_id: id.clone(),
                        retryable: true,
                        message: "retry".into(),
                    },
                    json!({"type":"failed","task_id":"task-1","retryable":true,"message":"retry"}),
                ),
                (
                    TaskEvent::Failed {
                        task_id: id,
                        retryable: false,
                        message: "stop".into(),
                    },
                    json!({"type":"failed","task_id":"task-1","retryable":false,"message":"stop"}),
                ),
            ];
            for (event, wire) in events {
                assert_eq!(serde_json::to_value(&event).unwrap(), wire);
                assert_eq!(serde_json::from_value::<TaskEvent>(wire).unwrap(), event);
            }
            assert!(
                serde_json::from_value::<TaskEvent>(json!({"type":"progress","task_id":"","message":"x"}))
                    .unwrap_err()
                    .to_string()
                    .contains("must not be empty")
            );
        }
    }
}
pub mod wyoming;
