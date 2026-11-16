//! Missing mute observations block privacy proof, never independent recovery.
use super::super::{Step, world::World};
use std::time::Duration;

pub const LIMITATION: &str = "PIN-1: live hassmic mute observation unavailable; native latch acknowledged but Muted was not observed within 2 seconds; no privacy counts claimed";

#[derive(Default)]
pub struct Probe {
    latched: bool,
    blocked: bool,
}

impl Probe {
    pub fn skipped(&self, step: &Step) -> bool {
        self.blocked
            && (self.latched
                && (matches!(step, Step::SayFile(_)) || matches!(step, Step::Ui(value) if value["state"] == "muted"))
                || matches!(step, Step::Trace(values) if !values.is_empty() && values.iter().all(|v| v.as_str().is_some_and(|s| s.starts_with("mic.mute_window")))))
    }

    pub fn pressed(&mut self, step: &Step) {
        if matches!(step, Step::Press(action) if action == "mute") {
            self.latched = !self.latched;
        }
    }

    pub async fn observe(&mut self, world: &mut World, step: &Step) -> Result<bool, String> {
        let Step::Ui(expected) = step else { return Ok(false) };
        if !self.latched || expected["state"] != "muted" {
            return Ok(false);
        }
        let screen = world.screen.as_mut().ok_or("M1: missing production screen observer")?;
        // A timeout provides no evidence of why the producer is unobserved.
        // It never passes privacy, and stream/auth/malformed errors still fail.
        if let Ok(result) = tokio::time::timeout(Duration::from_secs(2), screen.expect(expected)).await {
            result?;
        } else {
            self.blocked = true;
            world.mute_blocked = true;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lab_run::{Artifacts, Driver};
    use serde_json::json;

    // T3 local controlled SSE peer: absence blocks; cancellation retains a
    // partial wire frame so the next independent observation can still pass.
    #[tokio::test]
    async fn t3_missing_mute_observation_preserves_partial_screen_frame() {
        use tokio::{io::AsyncWriteExt, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\n\r\ndata: {\"seq\":1,\"state\":{\"state\":\"ready\",\"message\":\"Echo Live ready\"}}\n\ndata: {\"seq\":2,").await.unwrap();
            socket
        });
        let mut world = World::default();
        world.screen = Some(super::super::Screen::connect(address, true).await.unwrap());
        let mut socket = peer.await.unwrap();
        let mut probe = Probe {
            latched: true,
            blocked: false,
        };
        let step = Step::Ui(json!({"state":"muted"}));
        assert_eq!(probe.observe(&mut world, &step).await, Ok(true));
        assert!(world.mute_blocked);
        assert!(probe.skipped(&step));
        assert_eq!(world.screen.as_ref().unwrap().pending, b"data: {\"seq\":2,");
        socket
            .write_all(b"\"state\":{\"state\":\"speaking\"}}\n\n")
            .await
            .unwrap();
        assert_eq!(
            world
                .screen
                .as_mut()
                .unwrap()
                .expect(&json!({"state":"speaking"}))
                .await,
            Ok(())
        );
        drop(socket);
        // EOF is a real failure, even after an earlier unavailable observation.
        assert_eq!(
            probe.observe(&mut world, &step).await,
            Err("screen: stream ended".into())
        );
    }

    #[test]
    fn only_unproved_privacy_steps_are_skipped_and_unmute_is_always_executed() {
        let mut probe = Probe {
            latched: true,
            blocked: true,
        };
        assert!(probe.skipped(&Step::Ui(json!({"state":"muted"}))));
        assert!(probe.skipped(&Step::SayFile("fixture.pcm".into())));
        let unmute = Step::Press("mute".into());
        assert!(!probe.skipped(&unmute));
        probe.pressed(&unmute);
        assert!(!probe.skipped(&Step::Ui(json!({"state":"ready"}))));
        assert!(!probe.skipped(&Step::SayFile("fixture.pcm".into())));
        assert!(probe.skipped(&Step::Trace(vec![json!("mic.mute_window {received_frames: 0}")])));
        assert!(!probe.skipped(&Step::Trace(vec![json!("wyoming.run_started")])));
        for process in ["bridge", "hassmic-host"] {
            assert!(!probe.skipped(&Step::Restart {
                process: process.into(),
                count: 10,
                say: "fixture.pcm".into()
            }));
        }
    }

    struct Controlled {
        probe: Probe,
        restarts: u64,
    }
    impl Driver for Controlled {
        fn execute(
            &mut self,
            step: &Step,
            index: usize,
            _: Option<&Step>,
            artifacts: &mut Artifacts,
        ) -> Result<(), String> {
            if self.probe.skipped(step) {
                artifacts.blocked.insert(format!("step_{index}"), LIMITATION.into());
            } else {
                self.probe.pressed(step);
                if let Step::Restart { count, .. } = step {
                    self.restarts += count;
                }
            }
            Ok(())
        }
        fn close(&mut self, _: &mut Artifacts) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn blocked_dispatch_retains_independent_checks_and_publishes_nonpassing_report() {
        use crate::lab_run::{completed, dispatch};
        let root = std::env::temp_dir().join(format!("lab-blocked-dispatch-{}", std::process::id()));
        let mut artifacts = Artifacts::create(root.clone()).unwrap();
        let mut driver = Controlled {
            probe: Probe {
                latched: true,
                blocked: true,
            },
            restarts: 0,
        };
        let steps = [
            Step::Ui(json!({"state":"muted"})),
            Step::SayFile("fixture.pcm".into()),
            Step::Press("mute".into()),
            Step::Ui(json!({"state":"ready"})),
            Step::Restart {
                process: "bridge".into(),
                count: 10,
                say: "fixture.pcm".into(),
            },
            Step::Restart {
                process: "hassmic-host".into(),
                count: 10,
                say: "fixture.pcm".into(),
            },
        ];
        assert_eq!(dispatch(&mut driver, &steps, &mut artifacts), Ok(()));
        assert_eq!(driver.restarts, 20);
        assert_eq!(
            artifacts.checks,
            std::collections::BTreeMap::from([
                ("step_0".into(), false),
                ("step_1".into(), false),
                ("step_2".into(), true),
                ("step_3".into(), true),
                ("step_4".into(), true),
                ("step_5".into(), true)
            ])
        );
        assert!(
            completed(Ok(()), &artifacts, Some("scenario.yaml"))
                .unwrap_err()
                .contains("blocked")
        );
        artifacts.publish("M1", "T3", &"a".repeat(64), None, false).unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("report.json")).unwrap()).unwrap();
        assert_eq!(report["result"], "blocked");
        assert_eq!(artifacts.outcome(Some("restart failed")), "fail");
        assert_eq!(
            completed(Err("restart failed".into()), &artifacts, None),
            Err("restart failed".into())
        );
        drop(artifacts);
        std::fs::remove_dir_all(root).unwrap();
    }
}
