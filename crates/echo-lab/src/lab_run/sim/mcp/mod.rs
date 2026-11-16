//! M5 owns the production bridge, pinned host and strict loopback grant peers.
mod mcp_replay;
mod mcp_session;
mod mcp_wire;

use std::time::Duration;

use super::super::{Driver, McpAction, Step, run_options::Options, run_report::Artifacts};
use mcp_session::Session;

pub struct M5<'a> {
    options: &'a Options,
    runtime: tokio::runtime::Runtime,
    session: Session,
    deadline: tokio::time::Instant,
    expectations: Vec<(usize, Vec<super::selectors::Selector>)>,
}

impl<'a> M5<'a> {
    pub fn new(options: &'a Options) -> Result<Self, String> {
        Ok(Self {
            options,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "M5: cannot start runtime")?,
            session: Session::default(),
            deadline: tokio::time::Instant::now() + Duration::from_secs(120),
            expectations: vec![],
        })
    }

    async fn attempt(
        session: &mut Session,
        step: &Step,
        options: &Options,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        // Refuse unfinished capabilities before lazy startup, even for M5.
        match step {
            Step::Mcp(McpAction::Restart) if !session.started() => {
                return Err("M5: restart requires a running linked bridge".into());
            }
            Step::Mcp(McpAction::Replay) => session.admit_replay()?,
            Step::Mcp(
                McpAction::Link | McpAction::Approve | McpAction::Refresh | McpAction::Unlink | McpAction::Restart,
            )
            | Step::Ui(_)
            | Step::Trace(_) => {}
            Step::Mcp(action) => return Err(format!("M5: MCP {} control is not implemented", action.name())),
            _ => return Err("M5: unsupported step capability".into()),
        }
        if !session.started() {
            session.start(options, &artifacts.root).await?;
        }
        match step {
            Step::Mcp(McpAction::Link) => session.link().await?,
            Step::Mcp(McpAction::Approve) => session.approve().await?,
            Step::Mcp(McpAction::Refresh) => session.operator("--refresh-now", "MCP refreshed\n").await?,
            Step::Mcp(McpAction::Unlink) => session.operator("unlink", "MCP unlinked\n").await?,
            Step::Mcp(McpAction::Restart) => session.restart().await?,
            Step::Mcp(McpAction::Replay) => session.replay().await?,
            Step::Ui(expected) => super::observers::screen(
                &serde_json::to_vec(&session.screen).map_err(|_| "M5: screen encoding")?,
                expected,
            )?,
            Step::Trace(values) => {
                session.observe()?;
                super::selectors::check(&session.trace.records, &super::selectors::parse(values)?, None)?;
            }
            _ => return Err("M5: unsupported step capability".into()),
        }
        session.observe()?;
        artifacts.link_counts = Some(session.measure()?);
        Ok(())
    }
}

impl Driver for M5<'_> {
    fn execute(
        &mut self,
        step: &Step,
        index: usize,
        _: Option<&Step>,
        artifacts: &mut Artifacts,
    ) -> Result<(), String> {
        self.runtime.block_on(super::controlled(
            Self::attempt(&mut self.session, step, self.options, artifacts),
            self.options.cancel.as_deref(),
            self.deadline,
        ))?;
        if let Step::Trace(values) = step {
            self.expectations.push((index, super::selectors::parse(values)?));
        }
        Ok(())
    }

    fn close(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        let mut failures = self
            .runtime
            .block_on(self.session.close())
            .err()
            .into_iter()
            .collect::<Vec<_>>();
        for address in &self.session.endpoints {
            artifacts.endpoint(*address)?;
        }
        if let Some(measured) = self.session.final_measurement() {
            artifacts.link_counts = Some(measured);
        }
        for (index, selectors) in &self.expectations {
            if let Err(error) = self
                .session
                .trace
                .finish()
                .and_then(|()| super::selectors::check(&self.session.trace.records, selectors, None))
            {
                artifacts.checks.insert(format!("step_{index}"), false);
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}
