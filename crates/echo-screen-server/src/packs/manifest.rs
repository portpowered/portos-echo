use super::{Error, extension, relative};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub format: u8,
    pub name: String,
    pub version: String,
    pub layout: Option<Layout>,
    pub character: Option<Character>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Layout {
    safe_area: Area,
    slots: Slots,
    stage: Stage,
    css: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Area {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Slots {
    clock: Option<Slot>,
    date: Option<Slot>,
    status: Option<Slot>,
    link_code: Option<Slot>,
    approval: Option<Slot>,
    error: Option<Slot>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Slot {
    x: f64,
    y: f64,
    color: Option<String>,
    font_scale: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    anchor: Point,
    scale: f64,
    background: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Point {
    x: f64,
    y: f64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Character {
    pub rest: String,
    #[serde(default)]
    states: BTreeMap<String, StateClip>,
    #[serde(default)]
    emotes: BTreeMap<String, Clip>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StateClip {
    clip: Clip,
    enter: Option<Clip>,
    exit: Option<Clip>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Clip {
    webm: Option<String>,
    mp4: Option<String>,
    mode: Mode,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Loop,
    Once,
}

type Files = BTreeMap<String, Vec<u8>>;

pub(super) fn parse(files: &Files) -> Result<Manifest, Error> {
    let bytes = files.get("pack.json").ok_or(Error::Manifest)?;
    let pack: Manifest = serde_json::from_slice(bytes).map_err(|_| Error::Manifest)?;
    if pack.format != 1
        || !name(&pack.name)
        || !version(&pack.version)
        || (pack.layout.is_none() && pack.character.is_none())
    {
        return Err(Error::Manifest);
    }
    if let Some(layout) = &pack.layout {
        layout.validate(files)?;
    }
    if let Some(character) = &pack.character {
        character.validate(files)?;
    }
    // Validate every stylesheet, including unreferenced ones. No dormant unsafe CSS.
    for (path, bytes) in files {
        if extension(path)? == "css" {
            super::css::validate(std::str::from_utf8(bytes).map_err(|_| Error::Css)?, files)?;
        }
    }
    Ok(pack)
}

fn name(value: &str) -> bool {
    value.len() <= 48
        && value.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn version(value: &str) -> bool {
    let (release, pre) = value.split_once('-').map_or((value, None), |(v, p)| (v, Some(p)));
    let parts: Vec<_> = release.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && pre.is_none_or(|p| {
            p.split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric()))
        })
}

fn file(files: &Files, path: &str, types: &[&str]) -> Result<(), Error> {
    relative(path)?;
    if !files.contains_key(path) || !types.contains(&extension(path)?) {
        return Err(Error::Reference);
    }
    Ok(())
}

fn percent(value: f64) -> bool {
    value.is_finite() && (0.0..=100.0).contains(&value)
}

fn color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

impl Layout {
    fn validate(&self, files: &Files) -> Result<(), Error> {
        let a = &self.safe_area;
        if ![a.x, a.y, a.width, a.height].into_iter().all(percent)
            || a.width == 0.0
            || a.height == 0.0
            || a.x + a.width > 100.0
            || a.y + a.height > 100.0
            || !percent(self.stage.anchor.x)
            || !percent(self.stage.anchor.y)
            || !(0.0..=2.0).contains(&self.stage.scale)
            || self.stage.scale == 0.0
            || !color(&self.stage.background)
        {
            return Err(Error::Manifest);
        }
        let s = &self.slots;
        for slot in [&s.clock, &s.date, &s.status, &s.link_code, &s.approval, &s.error]
            .into_iter()
            .flatten()
        {
            if !percent(slot.x)
                || !percent(slot.y)
                || slot.color.as_deref().is_some_and(|c| !color(c))
                || slot.font_scale.is_some_and(|scale| !(0.5..=2.0).contains(&scale))
            {
                return Err(Error::Manifest);
            }
        }
        if let Some(path) = &self.css {
            file(files, path, &["css"])?;
        }
        Ok(())
    }
}

impl Character {
    fn validate(&self, files: &Files) -> Result<(), Error> {
        file(files, &self.rest, &["png", "webp"])?;
        for (state, clips) in &self.states {
            if ![
                "idle",
                "connecting",
                "listening",
                "thinking",
                "speaking",
                "muted",
                "setup",
                "approval",
                "error",
                "wake",
                "dnd",
                "timer",
                "call",
            ]
            .contains(&state.as_str())
            {
                return Err(Error::Manifest);
            }
            clips.clip.validate(files)?;
            for clip in [&clips.enter, &clips.exit].into_iter().flatten() {
                clip.validate(files)?;
            }
        }
        for (name, clip) in &self.emotes {
            if name.is_empty() || name.chars().count() > 128 {
                return Err(Error::Manifest);
            }
            clip.validate(files)?;
        }
        Ok(())
    }
}

impl Clip {
    fn validate(&self, files: &Files) -> Result<(), Error> {
        if self.webm.is_none() && self.mp4.is_none() {
            return Err(Error::Manifest);
        }
        if let Some(path) = &self.webm {
            file(files, path, &["webm"])?;
        }
        if let Some(path) = &self.mp4 {
            file(files, path, &["mp4"])?;
        }
        Ok(())
    }
}
