//! Which plugins use the microphone, camera or screen right now (Decision
//! 5). A provider that opens one calls [`Sensors::set`] for the calling
//! plugin and clears it when done; `bricolo status` lists them as
//! `active_sensors`, and the hub and menu bar show a dot with the name.
//! `mic` sets one while it records; the control socket's `sensor` request
//! sets one by hand to test that path.

use std::collections::BTreeSet;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensor {
    Mic,
    Camera,
    Screen,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActiveSensor {
    /// The plugin, or the script as `<name>.lua`.
    pub plugin: String,
    pub sensor: Sensor,
}

pub struct Sensors {
    active: Mutex<BTreeSet<(String, Sensor)>>,
    changed: watch::Sender<u64>,
}

impl Default for Sensors {
    fn default() -> Self {
        Self {
            active: Mutex::default(),
            changed: watch::channel(0).0,
        }
    }
}

impl Sensors {
    pub fn set(&self, plugin: &str, sensor: Sensor, active: bool) {
        let key = (plugin.to_string(), sensor);
        let changed = {
            let mut set = self.active.lock().unwrap();
            if active {
                set.insert(key)
            } else {
                set.remove(&key)
            }
        };
        if changed {
            self.changed.send_modify(|v| *v += 1);
        }
    }

    pub fn active(&self) -> Vec<ActiveSensor> {
        self.active
            .lock()
            .unwrap()
            .iter()
            .map(|(plugin, sensor)| ActiveSensor {
                plugin: plugin.clone(),
                sensor: *sensor,
            })
            .collect()
    }

    /// Changes each time a sensor turns on or off.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Clears what `plugin` had on. Called when it stops.
    pub fn release(&self, plugin: &str) {
        let changed = {
            let mut set = self.active.lock().unwrap();
            let before = set.len();
            set.retain(|(p, _)| p != plugin);
            set.len() != before
        };
        if changed {
            self.changed.send_modify(|v| *v += 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_list_and_release() {
        let sensors = Sensors::default();
        let changes = sensors.subscribe();
        sensors.set("dict", Sensor::Mic, true);
        sensors.set("dict", Sensor::Mic, true);
        sensors.set("cap.lua", Sensor::Screen, true);
        assert_eq!(*changes.borrow(), 2);
        assert_eq!(
            sensors.active(),
            vec![
                ActiveSensor {
                    plugin: "cap.lua".into(),
                    sensor: Sensor::Screen
                },
                ActiveSensor {
                    plugin: "dict".into(),
                    sensor: Sensor::Mic
                },
            ]
        );
        sensors.release("dict");
        sensors.set("cap.lua", Sensor::Screen, false);
        assert!(sensors.active().is_empty());
        assert_eq!(*changes.borrow(), 4);
    }

    #[test]
    fn serializes_lowercase() {
        let json = serde_json::to_string(&ActiveSensor {
            plugin: "p".into(),
            sensor: Sensor::Camera,
        })
        .unwrap();
        assert_eq!(json, r#"{"plugin":"p","sensor":"camera"}"#);
    }
}
