//! Home Assistant, over MQTT: the Launchpad as a device HA discovers by itself.
//!
//! - a **light** whose effect list is every effect plus [`PANEL`]: on starts one,
//!   off stops the session;
//! - a **select** for the audio source the sound effects listen to;
//! - an **event** entity, `event.launchpad_pad`, firing on every grid press and
//!   release with its `x`, `y` and velocity.
//!
//! HA colours the panel by publishing `{"pad": [x, y], "rgb": [r, g, b]}` (or a
//! list of them) to [`GRID`]; the Pi keeps no map of entities to pads, HA's
//! automations do. The Pi announces `online` on [`AVAILABILITY`] whenever it
//! connects, which is HA's cue to repaint, and the broker says `offline` for it
//! when the connection drops.
//!
//! Like the website, this only ever goes through [`runner`], so the one-session rule
//! and the stop that lets `Drop` run hold here too.

use std::time::Duration;

use flash_dance::Event;
use rumqttc::{AsyncClient, Event as Mqtt, LastWill, MqttOptions, Packet, QoS};
use serde_json::{Value, json};

use crate::app::Status;
use crate::runner::{self, PANEL};

const AVAILABILITY: &str = "flash-dance/availability";
const LIGHT_SET: &str = "flash-dance/light/set";
const LIGHT_STATE: &str = "flash-dance/light/state";
const SOURCE_SET: &str = "flash-dance/source/set";
const SOURCE_STATE: &str = "flash-dance/source/state";
const PAD: &str = "flash-dance/pad";
const GRID: &str = "flash-dance/grid/set";
const DISCOVERY: &str = "homeassistant/device/flash_dance/config";
/// HA's birth message: `online` here means it has (re)started and wants discovery again.
const HA_STATUS: &str = "homeassistant/status";

/// Where the broker is, from the environment. `FLASH_DANCE_MQTT` is `host` or
/// `host:port` (1883 by default); unset, the bridge does not run.
/// `FLASH_DANCE_MQTT_USER` and `FLASH_DANCE_MQTT_PASSWORD` log in.
pub fn options() -> Option<MqttOptions> {
    let (host, port) = address(&std::env::var("FLASH_DANCE_MQTT").ok()?)?;
    let mut options = MqttOptions::new("flash-dance", host, port);
    options
        .set_keep_alive(Duration::from_secs(30))
        // The discovery config carries every effect and source name; the default
        // 10 KB limit is closer than it looks.
        .set_max_packet_size(256 * 1024, 256 * 1024)
        .set_last_will(LastWill::new(
            AVAILABILITY,
            "offline",
            QoS::AtLeastOnce,
            true,
        ));
    if let Ok(user) = std::env::var("FLASH_DANCE_MQTT_USER") {
        let password = std::env::var("FLASH_DANCE_MQTT_PASSWORD").unwrap_or_default();
        options.set_credentials(user, password);
    }
    Some(options)
}

fn address(value: &str) -> Option<(String, u16)> {
    match value.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None if !value.is_empty() => Some((value.to_string(), 1883)),
        None => None,
    }
}

/// The bridge, forever: reconnecting when the broker goes away, publishing the
/// light's state when the session changes, whoever changed it.
pub async fn run(options: MqttOptions) {
    let (client, mut events) = AsyncClient::new(options, 64);
    let mut presses = runner::listen();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let sources: Vec<String> = runner::sources().into_iter().map(|s| s.name).collect();
    let mut source = sources.first().cloned().unwrap_or_default();
    // The effect "on" comes back to when HA sends no effect with it.
    let mut last = runner::effects()[0].name.clone();
    // `None` forces the next tick to publish, whatever the state is.
    let mut published: Option<Status> = None;

    loop {
        tokio::select! {
            event = events.poll() => match event {
                Ok(Mqtt::Incoming(Packet::ConnAck(_))) => {
                    println!("mqtt: connected");
                    // A fresh session every time, so subscriptions do not survive a
                    // reconnect and are made again here.
                    for topic in [LIGHT_SET, SOURCE_SET, GRID, HA_STATUS] {
                        let _ = client.try_subscribe(topic, QoS::AtLeastOnce);
                    }
                    announce(&client, &sources, &source);
                    published = None;
                }
                Ok(Mqtt::Incoming(Packet::Publish(message))) => {
                    match command(&message.topic, &message.payload) {
                        Some(Command::On(effect)) => {
                            let effect = effect.unwrap_or_else(|| last.clone());
                            last = effect.clone();
                            let source = source.clone();
                            // `start` joins the old session, which can take a frame.
                            let started = tokio::task::spawn_blocking(move || {
                                runner::start(&effect, Some(&source))
                            });
                            if let Ok(Err(error)) = started.await {
                                eprintln!("mqtt: {error}");
                            }
                            published = None;
                        }
                        Some(Command::Off) => {
                            let _ = tokio::task::spawn_blocking(runner::stop).await;
                            published = None;
                        }
                        Some(Command::Source(name)) if sources.contains(&name) => {
                            source = name;
                            publish(&client, SOURCE_STATE, true, source.clone());
                        }
                        Some(Command::Paint(pads)) => {
                            for (x, y, rgb) in pads {
                                runner::paint(x, y, rgb);
                            }
                        }
                        Some(Command::Announce) => {
                            announce(&client, &sources, &source);
                            published = None;
                        }
                        Some(Command::Source(_)) | None => {
                            eprintln!("mqtt: ignored a message on {}", message.topic);
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    // The next poll reconnects; wait so a dead broker is not hammered.
                    eprintln!("mqtt: {error}; retrying in 5 s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            },
            Some(event) = presses.recv() => {
                if let Some(payload) = press(event) {
                    publish(&client, PAD, false, payload.to_string());
                }
            }
            _ = tick.tick() => {
                // Polled rather than pushed, so it also catches a session ended on the
                // device's own quit button or started from the website.
                let status = runner::status();
                // Seen once: `status` collects an ended session, then reports idle.
                if let Status::Failed { effect, error } = &status {
                    eprintln!("mqtt: {effect} stopped: {error}");
                }
                if published.as_ref() != Some(&status) {
                    publish(&client, LIGHT_STATE, true, light_state(&status).to_string());
                    published = Some(status);
                }
            }
        }
    }
}

/// `try_publish`, not `publish`: the loop that drains the queue is the one calling,
/// so waiting for room in it would wait forever. A full queue drops the message.
fn publish(client: &AsyncClient, topic: &str, retain: bool, payload: String) {
    if let Err(error) = client.try_publish(topic, QoS::AtLeastOnce, retain, payload) {
        eprintln!("mqtt: could not publish on {topic}: {error}");
    }
}

/// Everything HA needs to (re)build the device, and the Pi's cue for it to repaint.
fn announce(client: &AsyncClient, sources: &[String], source: &str) {
    let names: Vec<String> = runner::effects().into_iter().map(|e| e.name).collect();
    publish(
        client,
        DISCOVERY,
        true,
        discovery(&names, sources).to_string(),
    );
    publish(client, AVAILABILITY, true, "online".to_string());
    if !source.is_empty() {
        publish(client, SOURCE_STATE, true, source.to_string());
    }
}

#[derive(Debug, PartialEq)]
enum Command {
    /// Light on, with the effect HA picked, if it picked one.
    On(Option<String>),
    Off,
    Source(String),
    Paint(Vec<(usize, usize, [u8; 3])>),
    /// HA restarted and has forgotten the device.
    Announce,
}

fn command(topic: &str, payload: &[u8]) -> Option<Command> {
    match topic {
        LIGHT_SET => {
            let light: Value = serde_json::from_slice(payload).ok()?;
            match light["state"].as_str()? {
                "ON" => Some(Command::On(light["effect"].as_str().map(String::from))),
                "OFF" => Some(Command::Off),
                _ => None,
            }
        }
        SOURCE_SET => Some(Command::Source(String::from_utf8(payload.to_vec()).ok()?)),
        GRID => {
            let value: Value = serde_json::from_slice(payload).ok()?;
            let pads = match value {
                Value::Array(pads) => pads,
                one => vec![one],
            };
            // One malformed entry spoils the message rather than painting half of it.
            pads.iter()
                .map(pad_colour)
                .collect::<Option<_>>()
                .map(Command::Paint)
        }
        HA_STATUS => (payload == b"online").then_some(Command::Announce),
        _ => None,
    }
}

fn pad_colour(value: &Value) -> Option<(usize, usize, [u8; 3])> {
    let byte = |v: &Value| v.as_u64().filter(|&c| c <= 255).map(|c| c as u8);
    let [x, y] = value["pad"].as_array()?.as_slice() else {
        return None;
    };
    let [r, g, b] = value["rgb"].as_array()?.as_slice() else {
        return None;
    };
    Some((
        x.as_u64()? as usize,
        y.as_u64()? as usize,
        [byte(r)?, byte(g)?, byte(b)?],
    ))
}

/// A grid press or release as HA's event entity wants it. The edge buttons are the
/// session's own controls, so they stay on the device.
fn press(event: Event) -> Option<Value> {
    let (x, y) = event.pad().to_xy()?;
    Some(match event {
        Event::Press(_, velocity) => {
            json!({"event_type": "press", "x": x, "y": y, "velocity": velocity})
        }
        Event::Release(_) => json!({"event_type": "release", "x": x, "y": y}),
    })
}

fn light_state(status: &Status) -> Value {
    match status {
        Status::Running(effect) => json!({"state": "ON", "effect": effect, "color_mode": "onoff"}),
        Status::Idle | Status::Failed { .. } => json!({"state": "OFF"}),
    }
}

/// HA's device discovery: one message for the device and all three entities.
/// `sources` empty (no sound server) leaves the select out.
fn discovery(effects: &[String], sources: &[String]) -> Value {
    let mut effect_list: Vec<&str> = effects.iter().map(String::as_str).collect();
    effect_list.push(PANEL);
    let mut components = json!({
        "light": {
            "platform": "light",
            "unique_id": "flash_dance_light",
            "name": null,
            "schema": "json",
            "command_topic": LIGHT_SET,
            "state_topic": LIGHT_STATE,
            "supported_color_modes": ["onoff"],
            "effect": true,
            "effect_list": effect_list,
        },
        "pad": {
            "platform": "event",
            "unique_id": "flash_dance_pad",
            "name": "Pad",
            "state_topic": PAD,
            "event_types": ["press", "release"],
        },
    });
    if !sources.is_empty() {
        components["source"] = json!({
            "platform": "select",
            "unique_id": "flash_dance_source",
            "name": "Audio source",
            "command_topic": SOURCE_SET,
            "state_topic": SOURCE_STATE,
            "options": sources,
        });
    }
    json!({
        "device": {
            "identifiers": ["flash_dance"],
            "name": "Launchpad",
            "manufacturer": "Novation",
            "model": "Launchpad Mini MK3",
        },
        "origin": {"name": "flash-dance", "url": "https://github.com/davidgomesdev/flash-dance"},
        "availability_topic": AVAILABILITY,
        "components": components,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flash_dance::Pad;

    #[test]
    fn address_defaults_the_port() {
        assert_eq!(address("ha.local"), Some(("ha.local".into(), 1883)));
        assert_eq!(address("10.0.0.2:1884"), Some(("10.0.0.2".into(), 1884)));
        assert_eq!(address("ha.local:nope"), None);
        assert_eq!(address(""), None);
    }

    #[test]
    fn light_commands() {
        assert_eq!(
            command(LIGHT_SET, br#"{"state":"ON","effect":"fire"}"#),
            Some(Command::On(Some("fire".into())))
        );
        assert_eq!(
            command(LIGHT_SET, br#"{"state":"ON"}"#),
            Some(Command::On(None))
        );
        assert_eq!(
            command(LIGHT_SET, br#"{"state":"OFF"}"#),
            Some(Command::Off)
        );
        assert_eq!(command(LIGHT_SET, b"not json"), None);
        assert_eq!(command(HA_STATUS, b"online"), Some(Command::Announce));
        assert_eq!(command(HA_STATUS, b"offline"), None);
    }

    #[test]
    fn grid_takes_one_pad_or_many_and_rejects_bad_colours() {
        assert_eq!(
            command(GRID, br#"{"pad":[0,7],"rgb":[255,180,100]}"#),
            Some(Command::Paint(vec![(0, 7, [255, 180, 100])]))
        );
        assert_eq!(
            command(
                GRID,
                br#"[{"pad":[1,0],"rgb":[0,0,0]},{"pad":[2,0],"rgb":[10,10,10]}]"#
            ),
            Some(Command::Paint(vec![(1, 0, [0; 3]), (2, 0, [10; 3])]))
        );
        assert_eq!(command(GRID, br#"{"pad":[0,0],"rgb":[256,0,0]}"#), None);
        assert_eq!(command(GRID, br#"{"pad":[0],"rgb":[1,2,3]}"#), None);
    }

    #[test]
    fn presses_carry_where_and_how_hard_and_edges_stay_home() {
        assert_eq!(
            press(Event::Press(Pad::xy(3, 5), 90)),
            Some(json!({"event_type": "press", "x": 3, "y": 5, "velocity": 90}))
        );
        assert_eq!(
            press(Event::Release(Pad::xy(0, 0))),
            Some(json!({"event_type": "release", "x": 0, "y": 0}))
        );
        assert_eq!(press(Event::Press(Pad::right(7), 127)), None);
    }

    #[test]
    fn discovery_offers_every_effect_and_the_panel() {
        let effects: Vec<String> = runner::effects().into_iter().map(|e| e.name).collect();
        let config = discovery(&effects, &[]);
        let list = config["components"]["light"]["effect_list"]
            .as_array()
            .unwrap();
        assert_eq!(list.len(), effects.len() + 1);
        assert_eq!(list.last().unwrap(), PANEL);
        assert!(effects.iter().all(|e| e != PANEL));
        assert!(config["components"].get("source").is_none());
        assert!(discovery(&effects, &["mic".into()])["components"]["source"].is_object());
    }
}
