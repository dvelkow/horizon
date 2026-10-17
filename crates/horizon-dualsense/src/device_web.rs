//! Browser gamepad input does not grant raw DualSense HID output access.
//! Keep the output-facing API available, but never report a connected device.
use crate::{TRIGGER_OFF, TriggerEffect};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Usb,
    Bluetooth,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    pub triggers: [TriggerEffect; 2],
    pub lightbar: Option<[u8; 3]>,
    pub player_leds: Option<u8>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            triggers: [TRIGGER_OFF; 2],
            lightbar: None,
            player_leds: None,
        }
    }
}

pub type Rumble = Vec<([u8; 2], Duration)>;

pub fn bluetooth_allowed() -> bool {
    false
}

pub struct Link;

impl Link {
    pub fn start() -> Self {
        log::info!("Horizon: raw DualSense output is unavailable in the browser");
        Self
    }
    pub fn open(&self) -> Option<Transport> {
        None
    }
    pub fn scan(&self) {}
    pub fn set(&self, _state: State) {}
    pub fn rumble(&self, _rumble: Rumble) {}
}
