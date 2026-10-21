//! Running effects, in this process, through `flash-dance-effects`.
//!
//! An effect is [`flash_dance_effects::run`] on a thread of its own — the same
//! session the `effects` binary runs, arrows and picker included — so the site
//! and the terminal cannot disagree about what an effect is or does.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use flash_dance::Event;
use flash_dance_effects::{EFFECTS, Picture, Report};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::app::{EffectInfo, SourceInfo, Status};

/// The session that is not an effect: the grid as a panel, showing [`PICTURE`].
/// Started by name like any effect, which no effect name collides with.
pub const PANEL: &str = "panel";

/// What the panel shows. Painted from outside (Home Assistant, over MQTT) whether
/// or not the panel is running, so switching to it shows the latest picture at once.
static PICTURE: Mutex<Picture> = Mutex::new([[[0; 3]; 8]; 8]);

/// Who hears the button events of whatever session is running.
static LISTENER: Mutex<Option<UnboundedSender<Event>>> = Mutex::new(None);

/// The one session. The Launchpad has a single MIDI port and a session holds it
/// open, so exactly one effect can run at a time — starting one stops the last.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

struct Running {
    /// What is on the grid now: the panel, or the effect last picked — on the
    /// device's own picker too, which is why the session reports it.
    showing: Arc<Mutex<&'static str>>,
    stop: Arc<AtomicBool>,
    /// The error is a `String` because the library's is not `Send`.
    thread: JoinHandle<Result<(), String>>,
}

fn running() -> MutexGuard<'static, Option<Running>> {
    // A panic while holding this lock would leave a session we still want to stop.
    lock(&RUNNING)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Sets pads of the panel's picture, `x` from the left and `y` from the top, under
/// one lock so the panel never shows half of them. Off-grid pads are ignored.
pub fn paint(pads: &[(usize, usize, [u8; 3])]) {
    let mut picture = lock(&PICTURE);
    for &(x, y, rgb) in pads {
        if let Some(pad) = picture.get_mut(y).and_then(|row| row.get_mut(x)) {
            *pad = rgb;
        }
    }
}

/// Every button event from now on, from every session. There is one listener;
/// a second call takes over from the first.
pub fn listen() -> UnboundedReceiver<Event> {
    let (tx, rx) = unbounded_channel();
    *lock(&LISTENER) = Some(tx);
    rx
}

/// Every effect, in picker order.
pub fn effects() -> Vec<EffectInfo> {
    EFFECTS
        .into_iter()
        .map(|effect| EffectInfo {
            group: effect.group().to_string(),
            name: effect.name().to_string(),
            about: effect.about().to_string(),
            // The hue its pad has in the device's own picker, so a card here is
            // the colour of the pad there.
            hue: effect.hue(),
        })
        .collect()
}

/// Capture sources, system sound first — that is what most people want to see the
/// sound effects react to.
pub fn sources() -> Vec<SourceInfo> {
    let mut sources: Vec<SourceInfo> = flash_dance_audio::sources()
        .unwrap_or_default()
        .into_iter()
        .map(|source| SourceInfo {
            name: source.name,
            monitor: source.monitor,
        })
        .collect();
    sources.sort_by_key(|source| !source.monitor);
    sources
}

/// Starts an effect by its exact name, or the panel as [`PANEL`], stopping whatever
/// was running first. `source` is the audio source for the sound effects; the
/// others ignore it.
pub fn start(name: &str, source: Option<&str>) -> Result<Status, String> {
    let effect = EFFECTS.into_iter().find(|effect| effect.name() == name);
    if effect.is_none() && name != PANEL {
        return Err(format!("there is no effect called {name:?}"));
    }
    let source = source.filter(|s| !s.is_empty()).map(String::from);

    let mut running = running();
    halt(&mut running);

    let stop = Arc::new(AtomicBool::new(false));
    let showing = Arc::new(Mutex::new(effect.map_or(PANEL, |e| e.name())));
    let (flag, shown) = (Arc::clone(&stop), Arc::clone(&showing));
    let report = move |report| match report {
        Report::Showing(effect) => *lock(&shown) = effect.name(),
        Report::Event(event) => {
            // Nobody listening, or the listener gone, is not the session's problem.
            if let Some(listener) = lock(&LISTENER).as_ref() {
                let _ = listener.send(event);
            }
        }
    };
    let thread = std::thread::Builder::new()
        .name(format!("effect: {name}"))
        .spawn(move || {
            match effect {
                Some(effect) => flash_dance_effects::run(effect, source.as_deref(), &flag, report),
                None => flash_dance_effects::panel(&PICTURE, &flag, report),
            }
            .map_err(|e| e.to_string())
        })
        .map_err(|e| e.to_string())?;
    *running = Some(Running {
        showing,
        stop,
        thread,
    });
    Ok(state(&mut running))
}

pub fn stop() -> Status {
    halt(&mut running());
    Status::Idle
}

pub fn status() -> Status {
    state(&mut running())
}

/// Sets the session's stop flag and waits for it to return, which takes at most a
/// frame. Waiting matters: returning is what runs the `Launchpad`'s `Drop`, which
/// clears the LEDs and hands the MIDI port back for the next effect to open.
fn halt(running: &mut Option<Running>) {
    if let Some(current) = running.take() {
        current.stop.store(true, Ordering::Relaxed);
        let _ = current.thread.join();
    }
}

/// What the session is doing, collecting it if it has ended. Ending cleanly is the
/// device's own Stop/Solo/Mute; anything else — no Launchpad plugged in, most
/// often — goes on the page.
fn state(running: &mut Option<Running>) -> Status {
    let Some(current) = running.as_ref() else {
        return Status::Idle;
    };
    let effect = lock(&current.showing).to_string();
    if !current.thread.is_finished() {
        return Status::Running(effect);
    }
    let ended = running.take().expect("checked above");
    match ended.thread.join() {
        Ok(Ok(())) => Status::Idle,
        Ok(Err(error)) => Status::Failed { effect, error },
        Err(_) => Status::Failed {
            effect,
            error: "the effect crashed".to_string(),
        },
    }
}
