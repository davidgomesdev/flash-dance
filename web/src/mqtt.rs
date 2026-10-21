//! Home Assistant, over MQTT: the Launchpad as a device HA discovers by itself.
//!
//! - a **light** whose effect list is every effect plus [`PANEL`]: on starts one,
//!   off stops the session;
//! - a **select** for the audio source the sound effects listen to;
//! - an **event** entity, `event.launchpad_pad`, firing on every grid press and
//!   release with its `x`, `y` and velocity;
//! - an **event** entity, `event.launchpad_mood`, firing with a mood's name as its
//!   event type when Session's mood pad is pressed.
//!
//! HA lists the moods, retained, on [`MOODS_SET`]: `[{"pad": [x, y], "name": …,
//! "rgb": [r, g, b]}]`. Their names are the mood entity's event types, so a new
//! list re-announces the device. A mood pressed stops the effect, and the light
//! says so: on, with a `null` effect.
//!
//! HA colours the panel by publishing `{"pad": [x, y], "rgb": [r, g, b]}` (or a
//! list of them) to [`GRID`]; the Pi keeps no map of entities to pads, HA's
//! automations do. The Pi announces `online` on [`AVAILABILITY`] whenever it
//! connects, which is HA's cue to repaint, and the broker says `offline` for it
//! when the connection drops.
//!
//! Like the website, this only ever goes through [`runner`], so the one-session rule
//! and the stop that lets `Drop` run hold here too.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use flash_dance::Event;
use flash_dance_effects::{Mood, Report};
use rumqttc::{AsyncClient, Event as Mqtt, LastWill, MqttOptions, Outgoing, Packet, QoS};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::app::Status;
use crate::runner::{self, PANEL};

const AVAILABILITY: &str = "flash-dance/availability";
const LIGHT_SET: &str = "flash-dance/light/set";
const LIGHT_STATE: &str = "flash-dance/light/state";
const SOURCE_SET: &str = "flash-dance/source/set";
const SOURCE_STATE: &str = "flash-dance/source/state";
const PAD: &str = "flash-dance/pad";
const GRID: &str = "flash-dance/grid/set";
const MOODS_SET: &str = "flash-dance/moods/set";
const MOOD: &str = "flash-dance/mood";
const DISCOVERY: &str = "homeassistant/device/flash_dance/config";
/// HA's birth message: `online` here means it has (re)started and wants discovery again.
const HA_STATUS: &str = "homeassistant/status";

/// Where the broker is, from the environment. `FLASH_DANCE_MQTT` is `host` or
/// `host:port` (1883 by default), an IPv6 address bracketed when it has a port, and
/// `mqtt://` in front is fine; unset, the bridge does not run.
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
    let value = value.strip_prefix("mqtt://").unwrap_or(value);
    if let Ok(socket) = value.parse::<SocketAddr>() {
        return Some((socket.ip().to_string(), socket.port()));
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Some((ip.to_string(), 1883));
    }
    match value.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None if !value.is_empty() => Some((value.to_string(), 1883)),
        None => None,
    }
}

/// The bridge, until `stop`: reconnecting when the broker goes away, publishing the
/// light's state when the session changes, whoever changed it.
pub async fn run(options: MqttOptions, mut stop: oneshot::Receiver<()>) {
    let (client, mut events) = AsyncClient::new(options, 64);
    let mut presses = runner::listen();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    // Sources are read again now and then: on a Pi the sound server can come up
    // after this does, and a mic can be plugged in later. The first tick is at once.
    let mut rescan = tokio::time::interval(Duration::from_secs(30));
    let mut sources: Vec<String> = Vec::new();
    let mut source = String::new();
    let mut moods: Vec<String> = Vec::new();
    // The effect "on" comes back to when HA sends no effect with it: whatever was
    // showing last, picked from HA, the website or the device's own picker.
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
                    for topic in [LIGHT_SET, SOURCE_SET, GRID, MOODS_SET, HA_STATUS] {
                        let _ = client.try_subscribe(topic, QoS::AtLeastOnce);
                    }
                    announce(&client, &sources, &source, &moods);
                    published = None;
                }
                Ok(Mqtt::Incoming(Packet::Publish(message))) => {
                    match command(&message.topic, &message.payload) {
                        Some(Command::On(effect)) => {
                            let effect = effect.unwrap_or_else(|| last.clone());
                            let source = source.clone();
                            // `start` joins the old session, which can take a frame.
                            let started = tokio::task::spawn_blocking(move || {
                                // HA sends "on" to a light already on (scenes do);
                                // restarting would reset its speed and brightness.
                                if runner::status() == Status::Running(effect.clone()) {
                                    return Ok(());
                                }
                                runner::start(&effect, Some(&source)).map(drop)
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
                        Some(Command::Paint(pads)) => runner::paint(&pads),
                        Some(Command::Moods(list)) => {
                            let names = event_types(&list);
                            runner::set_moods(list);
                            if names != moods {
                                moods = names;
                                announce(&client, &sources, &source, &moods);
                            }
                        }
                        Some(Command::Announce) => {
                            announce(&client, &sources, &source, &moods);
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
            Some(heard) = presses.recv() => match heard {
                Report::Event(event) => {
                    if let Some(payload) = press(event) {
                        publish(&client, PAD, false, payload.to_string());
                    }
                }
                Report::Mood(name) => {
                    publish(&client, MOOD, false, json!({"event_type": name}).to_string());
                }
                Report::Showing(_) => {}
            },
            _ = rescan.tick() => {
                let found = tokio::task::spawn_blocking(|| {
                    runner::sources().into_iter().map(|s| s.name).collect::<Vec<_>>()
                });
                let found = found.await.unwrap_or_default();
                if found != sources {
                    sources = found;
                    if !sources.contains(&source) {
                        source = sources.first().cloned().unwrap_or_default();
                    }
                    announce(&client, &sources, &source, &moods);
                }
            }
            _ = tick.tick() => {
                // Polled rather than pushed, so it also catches a session ended on the
                // device's own quit button or started from the website. Blocking, as
                // it waits for whoever is starting or stopping a session.
                let status = tokio::task::spawn_blocking(runner::status);
                let status = status.await.unwrap_or_default();
                if let Status::Running(effect) = &status {
                    last = effect.clone();
                }
                // Seen once: `status` collects an ended session, then reports idle.
                if let Status::Failed { effect, error } = &status {
                    eprintln!("mqtt: {effect} stopped: {error}");
                }
                if published.as_ref() != Some(&status) {
                    publish(&client, LIGHT_STATE, true, light_state(&status).to_string());
                    published = Some(status);
                }
            }
            _ = &mut stop => {
                // A clean disconnect sends no last will, so say it here: off, and gone.
                publish(&client, LIGHT_STATE, true, light_state(&Status::Idle).to_string());
                publish(&client, AVAILABILITY, true, "offline".to_string());
                let _ = client.try_disconnect();
                let _ = tokio::time::timeout(Duration::from_secs(1), async {
                    while let Ok(event) = events.poll().await {
                        if matches!(event, Mqtt::Outgoing(Outgoing::Disconnect)) {
                            break;
                        }
                    }
                })
                .await;
                return;
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
fn announce(client: &AsyncClient, sources: &[String], source: &str, moods: &[String]) {
    let names: Vec<String> = runner::effects().into_iter().map(|e| e.name).collect();
    publish(
        client,
        DISCOVERY,
        true,
        discovery(&names, sources, moods).to_string(),
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
    Moods(Vec<Mood>),
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
        MOODS_SET => {
            let value: Value = serde_json::from_slice(payload).ok()?;
            // Like the grid: one bad mood spoils the list.
            value
                .as_array()?
                .iter()
                .map(mood)
                .collect::<Option<_>>()
                .map(Command::Moods)
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

fn mood(value: &Value) -> Option<Mood> {
    let byte = |v: &Value| v.as_u64().filter(|&c| c <= 255).map(|c| c as u8);
    let on_grid = |v: &Value| v.as_u64().filter(|&c| c < 8).map(|c| c as u8);
    let [x, y] = value["pad"].as_array()?.as_slice() else {
        return None;
    };
    let [r, g, b] = value["rgb"].as_array()?.as_slice() else {
        return None;
    };
    let name = value["name"].as_str().filter(|n| !n.is_empty())?;
    Some(Mood {
        x: on_grid(x)?,
        y: on_grid(y)?,
        name: name.to_string(),
        rgb: [byte(r)?, byte(g)?, byte(b)?],
    })
}

/// The mood entity's event types: each name once, in the order HA listed them.
fn event_types(moods: &[Mood]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for mood in moods {
        if !names.contains(&mood.name) {
            names.push(mood.name.clone());
        }
    }
    names
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
        // `null`, not left out: HA keeps the last effect when the key is missing.
        Status::Mood(_) => {
            json!({"state": "ON", "effect": null, "color_mode": "onoff"})
        }
        Status::Running(effect) => json!({"state": "ON", "effect": effect, "color_mode": "onoff"}),
        Status::Idle | Status::Failed { .. } => json!({"state": "OFF"}),
    }
}

/// HA's device discovery: one message for the device and all its entities.
/// `sources` empty (no sound server) leaves the select out, `moods` empty the
/// mood entity.
fn discovery(effects: &[String], sources: &[String], moods: &[String]) -> Value {
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
    if !moods.is_empty() {
        components["mood"] = json!({
            "platform": "event",
            "unique_id": "flash_dance_mood",
            "name": "Mood",
            "state_topic": MOOD,
            "event_types": moods,
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
        assert_eq!(
            address("mqtt://ha.local:1883"),
            Some(("ha.local".into(), 1883))
        );
        assert_eq!(address("fd00::5"), Some(("fd00::5".into(), 1883)));
        assert_eq!(address("[fd00::5]:1884"), Some(("fd00::5".into(), 1884)));
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
        let config = discovery(&effects, &[], &[]);
        let list = config["components"]["light"]["effect_list"]
            .as_array()
            .unwrap();
        assert_eq!(list.len(), effects.len() + 1);
        assert_eq!(list.last().unwrap(), PANEL);
        assert!(effects.iter().all(|e| e != PANEL));
        assert!(config["components"].get("source").is_none());
        assert!(config["components"].get("mood").is_none());
        let full = discovery(&effects, &["mic".into()], &["Party".into()]);
        assert!(full["components"]["source"].is_object());
        assert_eq!(full["components"]["mood"]["event_types"], json!(["Party"]));
    }

    #[test]
    fn moods_need_a_grid_pad_a_name_and_a_colour() {
        let party = Mood {
            x: 1,
            y: 7,
            name: "Party".into(),
            rgb: [255, 0, 128],
        };
        assert_eq!(
            command(
                MOODS_SET,
                br#"[{"pad":[1,7],"name":"Party","rgb":[255,0,128]}]"#
            ),
            Some(Command::Moods(vec![party.clone()]))
        );
        assert_eq!(command(MOODS_SET, b"[]"), Some(Command::Moods(vec![])));
        assert_eq!(
            command(MOODS_SET, br#"[{"pad":[8,0],"name":"x","rgb":[0,0,0]}]"#),
            None
        );
        assert_eq!(
            command(MOODS_SET, br#"[{"pad":[0,0],"name":"","rgb":[0,0,0]}]"#),
            None
        );
        assert_eq!(
            command(MOODS_SET, br#"{"pad":[0,0],"name":"x","rgb":[0,0,0]}"#),
            None
        );
        assert_eq!(
            event_types(&[party.clone(), party]),
            vec!["Party".to_string()]
        );
    }

    #[test]
    fn a_mood_turns_the_effect_off_explicitly() {
        assert_eq!(
            light_state(&Status::Mood("Party".into())),
            json!({"state": "ON", "effect": null, "color_mode": "onoff"})
        );
    }
}
