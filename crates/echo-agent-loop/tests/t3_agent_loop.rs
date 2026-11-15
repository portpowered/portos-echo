//! T3 component tests through the public `AgentBackend` boundary.
#[cfg(test)]
mod cases;

#[cfg(test)]
mod tests {
    use super::cases;
    use echo_config::LiveMode;

    #[tokio::test]
    async fn f01_one_tool_each_mode() {
        for mode in [LiveMode::Chatgpt, LiveMode::ApiKey] {
            cases::journey("F01", mode, 1).await;
        }
    }
}
