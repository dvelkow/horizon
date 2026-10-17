//! Browser host facade. Guest code runs only in isolated JavaScript Workers;
//! this facade exchanges snapshots and bounded retained output with their host.
use crate::{Manifest, Nudge};
use horizon_platform::{
    path::{Path, PathBuf},
    time::Duration,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(
    inline_js = "export function horizon_mod_dispatch(method, payload) { const host = globalThis.horizonMods; return host ? JSON.stringify(host.dispatch(method, JSON.parse(payload))) : '{}'; }"
)]
extern "C" {
    fn horizon_mod_dispatch(method: &str, payload: &str) -> String;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub health: f32,
    pub food: u32,
    pub dimension: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clock {
    pub game_time: u64,
    pub day_time: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Motion {
    pub vx: f64,
    pub vy: f64,
    pub vz: f64,
    pub on_ground: bool,
    pub jumping: bool,
    pub steerable: bool,
    pub lift_left: u32,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub player: Option<Player>,
    pub clock: Option<Clock>,
    pub motion: Option<Motion>,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Line {
    pub text: String,
    pub color: u32,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Panel {
    pub corner: Corner,
    pub lines: Vec<Line>,
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Output {
    Chat { name: String, text: String },
    Feedback { text: String, error: bool },
    Stopped { name: String, reason: String },
}
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Off,
    Running,
    Failed(String),
}
pub struct Entry {
    pub path: PathBuf,
    pub manifest: Option<Manifest>,
    pub status: Status,
    pub cost: Duration,
}
#[derive(Default)]
pub struct Mods {
    entries: Vec<Entry>,
    panels: Vec<Panel>,
    commands: Vec<String>,
    out: Vec<Output>,
    nudge: Nudge,
    revision: u64,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct View {
    /// Each a [`ViewEntry`], read one by one: an entry this build cannot
    /// read is left out instead of losing the whole view.
    entries: Vec<serde_json::Value>,
    panels: Vec<Panel>,
    commands: Vec<String>,
    out: Vec<Output>,
    nudge: Option<ViewNudge>,
    revision: u64,
}
/// The host's merged `movement` nudges; bounded again on this side.
#[derive(Default, Deserialize)]
#[serde(default)]
struct ViewNudge {
    vertical: Option<f64>,
    push: [f64; 2],
}
#[derive(Deserialize)]
struct ViewEntry {
    manifest: Manifest,
    status: String,
    #[serde(default)]
    error: String,
}

pub fn find(_dir: &Path) -> Vec<PathBuf> {
    Vec::new()
}
impl Mods {
    pub fn load(
        _paths: &[PathBuf],
        _enabled: impl Fn(&str) -> bool,
        reserved: impl Fn(&str) -> bool,
    ) -> Self {
        let mut host = Self::default();
        host.reload_changed(reserved);
        host
    }
    fn call(&mut self, method: &str, payload: serde_json::Value) -> serde_json::Value {
        serde_json::from_str(&horizon_mod_dispatch(method, &payload.to_string()))
            .unwrap_or_default()
    }
    pub fn is_running(&self) -> bool {
        self.entries.iter().any(|e| e.status == Status::Running)
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.call(
            "snapshot",
            serde_json::to_value(snapshot).unwrap_or_default(),
        );
    }
    pub fn tick(&mut self, tick: u64) {
        self.call("tick", serde_json::json!(tick.to_string()));
    }
    pub fn frame(&mut self, seconds: f32) {
        self.call("frame", serde_json::json!(seconds));
    }
    pub fn chat(&mut self, text: &str) {
        self.call("chat", serde_json::json!(text));
    }
    pub fn has_command(&self, name: &str) -> bool {
        self.commands.iter().any(|c| c == name)
    }
    pub fn command(&mut self, line: &str) -> bool {
        let name = line
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        if !self.has_command(name) {
            return false;
        }
        self.call("command", serde_json::json!(line));
        true
    }
    pub fn commands(&self) -> impl Iterator<Item = &str> {
        self.commands.iter().map(String::as_str)
    }
    pub fn panels(&self) -> impl Iterator<Item = &Panel> {
        self.panels.iter()
    }
    pub fn drain(&mut self) -> Vec<Output> {
        if let Ok(view) = serde_json::from_value::<View>(self.call("poll", serde_json::Value::Null))
        {
            self.panels = view
                .panels
                .into_iter()
                .map(|mut p| {
                    p.lines.truncate(16);
                    for l in &mut p.lines {
                        l.text = l.text.chars().take(256).collect();
                    }
                    p
                })
                .collect();
            self.out.extend(view.out.into_iter().take(64));
            if let Some(n) = view.nudge {
                let mut later = Nudge::default();
                if let Some(vy) = n.vertical {
                    later.set_vertical(vy);
                }
                later.add_push(n.push[0], n.push[1]);
                self.nudge.merge(later);
            }
        }
        std::mem::take(&mut self.out)
    }
    pub fn take_nudge(&mut self) -> Nudge {
        std::mem::take(&mut self.nudge)
    }
    pub fn reload_changed(&mut self, reserved: impl Fn(&str) -> bool) -> bool {
        let Ok(view) = serde_json::from_value::<View>(self.call("view", serde_json::Value::Null))
        else {
            return false;
        };
        if view.revision == self.revision {
            return false;
        }
        self.revision = view.revision;
        self.commands = view
            .commands
            .into_iter()
            .filter(|n| !reserved(n))
            .take(128)
            .collect();
        self.entries = view
            .entries
            .into_iter()
            .filter_map(|e| serde_json::from_value::<ViewEntry>(e).ok())
            .take(32)
            .map(|e| Entry {
                path: PathBuf::from(format!("/approved-mods/{}.hmod", e.manifest.id)),
                manifest: Some(e.manifest),
                cost: Duration::ZERO,
                status: match e.status.as_str() {
                    "running" => Status::Running,
                    "off" => Status::Off,
                    _ => Status::Failed(e.error),
                },
            })
            .collect();
        true
    }
}
