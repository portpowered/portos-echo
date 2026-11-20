//! M4 scenario adapter: each case owns one fully drained production journey.
use std::time::Duration;

#[cfg(test)]
use echo_live::fake::{DialogueCall, DialogueSent};
use serde_json::{Value, json};

use super::super::{observers, selectors};
use super::{golden, wire, world::World};
use crate::lab_run::{Driver, Step, run_options::Options, run_report::Artifacts};

const UTTERANCE: &str = "Computer, call the test tool and tell me its result";

pub fn utterance(value: &str) -> Result<Step, String> {
    if value != UTTERANCE {
        return Err("m4: no finite synthetic fixture for this utterance".into());
    }
    Ok(Step::SayText(value.into()))
}

pub fn emotion_utterance(value: &str) -> Result<Step, String> {
    if value != super::emotion::UTTERANCE {
        return Err("m6: no finite synthetic fixture for this utterance".into());
    }
    Ok(Step::SayText(value.into()))
}

pub fn negative(scenario: &Value) -> Result<Vec<Step>, String> {
    if scenario["id"] != "m4-spoof-tool" {
        return Ok(vec![]);
    }
    let cases = scenario["negative"].as_array().ok_or("m4: missing negative cases")?;
    if cases.len() != 1 || cases[0]["inject"] != json!({"mcp":"http_500"}) {
        return Err("m4: expected one explicit http_500 case".into());
    }
    let mut steps = vec![Step::Inject];
    for (name, value) in cases[0].as_object().ok_or("m4: invalid negative case")? {
        match name.as_str() {
            "inject" => {}
            "expect_trace_absent" => {
                let values = value.as_array().ok_or("m4: invalid absent selectors")?;
                selectors::parse(values)?;
                steps.push(Step::TraceAbsent(values.clone()));
            }
            _ => steps.push(super::super::parse(&json!({name:value}))?),
        }
    }
    Ok(steps)
}

pub struct M4<'a> {
    options: &'a Options,
    runtime: tokio::runtime::Runtime,
    world: World,
    run: Option<String>,
    transcript: Option<String>,
    samples: Vec<Duration>,
    negative: bool,
    completed: bool,
    emotion: bool,
}

impl<'a> M4<'a> {
    pub fn new(options: &'a Options) -> Result<Self, String> {
        Ok(Self {
            emotion: crate::lab_validate::load(&options.scenarios[0])?["id"] == "m6-emotion",
            options,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "m4: cannot start runtime")?,
            world: World::default(),
            run: None,
            transcript: None,
            samples: vec![],
            negative: false,
            completed: false,
        })
    }

    fn journey(&mut self, artifacts: &mut Artifacts) -> Result<(), String> {
        self.completed = false;
        self.transcript = None;
        self.run = None;
        let journey = self
            .runtime
            .block_on(Box::pin(super::codex::run(self.options, self.negative)))?;
        self.world.trace = journey.trace;
        self.world.speaking = journey.speaking;
        self.world.ui_states = journey.ui_states;
        self.world.reply_pcm_bytes = journey.timing.played;
        self.world.playback = journey.playback;
        self.samples.push(journey.timing.sample()?);
        self.run = Some(
            journey.completion["run_id"]
                .as_str()
                .ok_or("m4: missing completed run")?
                .into(),
        );
        self.transcript = Some(journey.text);
        self.completed = true;
        let case = if self.negative { "http500" } else { "success" };
        self.record_audio(artifacts, case, &journey.timing)?;
        if self.emotion {
            artifacts.m6 = Some(super::emotion::measurement(&self.world));
            super::emotion::ordered(&self.world.ui_states, &json!({"sequence":super::emotion::sequence()}))
        } else {
            self.record(artifacts, case, &journey.timing)
        }
    }

    fn record_audio(&self, artifacts: &mut Artifacts, case: &str, timing: &wire::Timing) -> Result<(), String> {
        let host = self.world.playback.as_ref().ok_or("m4: missing host playback")?;
        let audio = json!({"case":case,"run_id":self.run,
            "audibility":timing.audible_reply(host)?,"ui_states":self.world.ui_states});
        let milestone = if self.emotion { "m6" } else { "m4" };
        let name = format!("{milestone}-{case}-audible.json");
        std::fs::write(artifacts.root.join(&name), audio.to_string())
            .map_err(|_| "m4: cannot retain audible playback observations")?;
        artifacts.evidence.push(name);
        Ok(())
    }

    fn record(&self, artifacts: &mut Artifacts, case: &str, timing: &wire::Timing) -> Result<(), String> {
        let name = format!("m4-{case}.jsonl");
        let lines = golden::normalize(&self.world.trace)?.join("\n") + "\n";
        std::fs::write(artifacts.root.join(&name), lines).map_err(|_| "m4: cannot retain normalized trace")?;
        artifacts.evidence.push(name.clone());
        let delegation = self
            .world
            .trace
            .records
            .iter()
            .find(|record| record["event"] == "delegation.created" && record["source"] == "provider.sideband")
            .and_then(|record| record["delegation_id"].as_str())
            .ok_or("m4: missing native delegation identity")?;
        let evidence = json!({"event":"m4.case","case":case,"run_id":self.run,"delegation_id":delegation,
            "tools_call_count":1,"commentary_count":1,"screen":"speaking",
            "transcript":self.transcript,"capture_bytes":timing.captured,"reply_pcm_bytes":timing.played,
            "endpoint_to_first_pcm_us":timing.sample()?.as_micros()});
        artifacts.measurement(evidence)?;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/golden")
            .join(name);
        let bytes = std::fs::read(path).map_err(|_| "golden: missing recorded M4 fixture")?;
        golden::compare(&self.world.trace, &bytes)
    }

    fn check(&self, step: &Step) -> Result<(), String> {
        if !self.completed {
            return Err("m4: missing completed current-case observations".into());
        }
        let records = golden::milestone_records(&self.world.trace);
        match step {
            Step::Trace(values) => selectors::check(&records, &selectors::parse(values)?, self.run.as_deref()),
            Step::TraceAbsent(values) => {
                for value in values {
                    if selectors::check(
                        &records,
                        &selectors::parse(std::slice::from_ref(value))?,
                        self.run.as_deref(),
                    )
                    .is_ok()
                    {
                        return Err("m4: forbidden trace occurrence observed".into());
                    }
                }
                Ok(())
            }
            Step::Speech(phrases) => crate::lab_run::run_oracles::speech(
                self.transcript
                    .as_deref()
                    .ok_or("m4: missing sent transcript")?
                    .as_bytes(),
                phrases,
            ),
            Step::Ui(expected) if self.emotion => super::emotion::ordered(&self.world.ui_states, expected),
            Step::Ui(expected) => observers::screen(
                &serde_json::to_vec(
                    self.world
                        .speaking
                        .as_ref()
                        .ok_or("m4: missing current screen observation")?,
                )
                .map_err(|_| "m4: invalid screen observation")?,
                expected,
            ),
            Step::Budget(name, ceiling) if name == "first_audio_after_endpoint_ms_p95" => {
                let measured = wire::p95(&self.samples)?.as_secs_f64() * 1000.0;
                if measured > *ceiling {
                    return Err(format!("m4: p95 {measured} exceeds {ceiling} ms"));
                }
                Ok(())
            }
            _ => Err("m4: unsupported scenario step".into()),
        }
    }
}

impl Driver for M4<'_> {
    fn execute(&mut self, step: &Step, _: usize, _: Option<&Step>, artifacts: &mut Artifacts) -> Result<(), String> {
        match step {
            Step::SayText(text) if !self.completed && !self.negative => {
                if self.emotion {
                    emotion_utterance(text)?;
                } else {
                    utterance(text)?;
                }
                self.journey(artifacts)
            }
            Step::Inject if self.completed && !self.negative => {
                self.negative = true;
                self.journey(artifacts)
            }
            _ => self.check(step),
        }
    }

    fn close(&mut self, _: &mut Artifacts) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
fn sent_transcript(calls: &[DialogueCall]) -> Result<String, String> {
    if calls.len() != 1
        || calls[0].call_index != 0
        || calls[0].delegation_id != "d-1"
        || calls[0].failure.is_some()
        || calls[0].rejected_commentary != 0
        || !calls[0].reply_completed
    {
        return Err("m4: invalid or mismatched dialogue observation".into());
    }
    let transcripts = calls[0]
        .sent
        .iter()
        .filter_map(|sent| match sent {
            DialogueSent::Text(text) => serde_json::from_str::<Value>(text)
                .ok()
                .filter(|v| v["type"] == "response.audio_transcript.done"),
            DialogueSent::Audio(_) => None,
        })
        .collect::<Vec<_>>();
    if transcripts.len() != 1 {
        return Err("m4: expected exactly one sent transcript".into());
    }
    transcripts[0]["transcript"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "m4: missing transcript text".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call() -> DialogueCall {
        DialogueCall {
            call_index: 0,
            delegation_id: "d-1".into(),
            sent: vec![DialogueSent::Text(
                json!({"type":"response.audio_transcript.done","transcript":"test echo"}).to_string(),
            )],
            rejected_commentary: 0,
            received_audio: 1,
            reply_completed: true,
            closed: true,
            failure: None,
        }
    }

    #[test]
    fn missing_stale_wrong_call_or_failed_transcripts_cannot_supply_speech() {
        assert_eq!(sent_transcript(&[call()]), Ok("test echo".into()));
        assert!(sent_transcript(&[]).is_err());
        assert!(sent_transcript(&[call(), call()]).is_err());
        for index in 0..7 {
            let mut wrong = call();
            match index {
                0 => wrong.call_index = 1,
                1 => wrong.delegation_id = "stale".into(),
                2 => wrong.failure = Some("decode".into()),
                3 => wrong.reply_completed = false,
                4 => wrong.rejected_commentary = 1,
                5 => wrong.sent.clear(),
                _ => wrong.sent.push(wrong.sent[0].clone()),
            }
            assert!(sent_transcript(&[wrong]).is_err());
        }
    }

    #[test]
    fn scenario_mapping_requires_unchanged_utterance_and_explicit_fault() {
        assert!(matches!(utterance(UTTERANCE), Ok(Step::SayText(_))));
        assert!(utterance("unmapped").is_err());
        assert!(negative(&json!({"id":"w1"})).unwrap().is_empty());
        for cases in [
            json!([]),
            json!([{"inject":{"mcp":"timeout"}}]),
            json!([{"inject":{"mcp":"http_500"},"expect_trace_absent":[]}]),
        ] {
            assert!(negative(&json!({"id":"m4-spoof-tool","negative":cases})).is_err());
        }
        let steps = negative(&json!({"id":"m4-spoof-tool","negative":[{"inject":{"mcp":"http_500"},
            "expect_trace_absent":["tools/call {count: 2}"],"expect_speech_contains":["couldn't"]}]}))
        .unwrap();
        assert_eq!(steps.len(), 3);
        assert!(matches!(steps[0], Step::Inject));
    }
}
