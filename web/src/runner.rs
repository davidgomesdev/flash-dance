//! Running effects, in this process, through `flash-dance-effects`.
//!
//! An effect is [`flash_dance_effects::run`] on a thread of its own — the same
//! session the `effects` binary runs, arrows and picker included — so the site
//! and the terminal cannot disagree about what an effect is or does.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use flash_dance_effects::{EFFECTS, Marquee, Mood, Moods, Picture, Report};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::app::{EffectInfo, MoodInfo, SourceInfo, Status};

/// The session that is not an effect, showing [`PICTURE`]; defined beside
/// [`Status`] so the page can tell it apart too.
pub use crate::app::{DARK, PANEL};

/// What the panel shows. Painted from outside (Home Assistant, over MQTT) whether
/// or not the panel is running, so switching to it shows the latest picture at once.
static PICTURE: Mutex<Picture> = Mutex::new([[[0; 3]; 8]; 8]);

/// What Session shows, and a mood pressed on the website. Set from outside like
/// [`PICTURE`], and read by every session.
static MOODS: Mutex<Moods> = Mutex::new(Moods::new());

/// Text waiting for the running effect session to take it, next frame.
static MARQUEE: Mutex<Option<Marquee>> = Mutex::new(None);

/// Who hears the button events and mood presses of whatever session is running.
static LISTENER: Mutex<Option<UnboundedSender<Report>>> = Mutex::new(None);

/// The last error listing audio sources, so [`sources`] warns only when it changes.
static SOURCES_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// The one session. The Launchpad has a single MIDI port and a session holds it
/// open, so exactly one effect can run at a time — starting one stops the last.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// While set, [`supervise`] keeps a session on the device: the server always
/// controls the Launchpad, falling back to the panel. Cleared by [`shutdown`].
static SUPERVISING: AtomicBool = AtomicBool::new(false);

/// How often the supervisor looks, and so how soon a plugged-in Launchpad lights.
const RETRY: Duration = Duration::from_secs(1);

/// The last session that failed and its error, until a session stays up: what the
/// page and Home Assistant are told while there is no Launchpad, and how the
/// supervisor knows a failure is a repeat, logged at debug rather than error.
static FAILURE: Mutex<Option<(String, String)>> = Mutex::new(None);

struct Running {
    /// What is on the grid now, as [`Status::Running`] or [`Status::Mood`]: the
    /// panel, the effect last picked — on the device's own picker too, which is why
    /// the session reports it — or the mood last pressed.
    showing: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
    /// The error is a `String` because the library's is not `Send`.
    thread: JoinHandle<Result<(), String>>,
    /// A session alive [`RETRY`] after this has connected: a missing Launchpad
    /// fails within milliseconds.
    started: Instant,
    /// What was started, by name, and its audio source: what the supervisor tries
    /// again when the session failed, so a Launchpad plugged back in comes back to
    /// it — dark stays dark — rather than to the panel.
    wanted: (String, Option<String>),
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
    tracing::debug!(pads = pads.len(), "painting the panel");
    let mut picture = lock(&PICTURE);
    for &(x, y, rgb) in pads {
        if let Some(pad) = picture.get_mut(y).and_then(|row| row.get_mut(x)) {
            *pad = rgb;
        }
    }
}

/// Replaces the moods Session shows; a running session shows the new list at once.
pub fn set_moods(moods: Vec<Mood>) {
    let names: Vec<&str> = moods.iter().map(|mood| mood.name.as_str()).collect();
    tracing::info!(?names, "moods set");
    lock(&MOODS).list = moods;
}

/// Every button event, mood press and tap of Stop from now on, from every session,
/// as [`Report::Event`], [`Report::Mood`] and [`Report::Stop`]. There is one listener; a second call
/// takes over from the first.
pub fn listen() -> UnboundedReceiver<Report> {
    let (tx, rx) = unbounded_channel();
    *lock(&LISTENER) = Some(tx);
    rx
}

/// The moods Session shows, in the order Home Assistant listed them.
pub fn moods() -> Vec<MoodInfo> {
    lock(&MOODS)
        .list
        .iter()
        .map(|mood| MoodInfo {
            name: mood.name.clone(),
            rgb: mood.rgb,
        })
        .collect()
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
    let listed = flash_dance_audio::sources().map_err(|error| error.to_string());
    // Said once per change: the bridge rescans every 30 s, and a machine with no
    // sound server would otherwise warn on every one.
    match news(&mut lock(&SOURCES_ERROR), listed.as_ref().err()) {
        Some(News::Failing(error)) => tracing::warn!("cannot list audio sources: {error}"),
        Some(News::Recovered) => tracing::info!("audio sources can be listed again"),
        None => {}
    }
    let mut sources: Vec<SourceInfo> = listed
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

#[derive(Debug, PartialEq)]
enum News {
    Failing(String),
    Recovered,
}

/// What is worth saying about this listing, given the error the last one had:
/// a new or different error, or the first success after one. `last` becomes this
/// listing's error.
fn news(last: &mut Option<String>, error: Option<&String>) -> Option<News> {
    let said = match (error, last.as_ref()) {
        (Some(error), seen) if seen != Some(error) => Some(News::Failing(error.clone())),
        (None, Some(_)) => Some(News::Recovered),
        _ => None,
    };
    *last = error.cloned();
    said
}

/// Starts an effect by its exact name, the panel as [`PANEL`] or the dark grid as
/// [`DARK`], stopping whatever
/// was running first. `source` is the audio source for the sound effects; the
/// others ignore it.
pub fn start(name: &str, source: Option<&str>) -> Result<Status, String> {
    let mut running = running();
    start_on(&mut running, name, source, false)?;
    Ok(state(&running))
}

/// [`start`], under the lock the caller holds. `retry` is the supervisor trying
/// again after a failure, which says so at debug rather than info.
fn start_on(
    running: &mut Option<Running>,
    name: &str,
    source: Option<&str>,
    retry: bool,
) -> Result<(), String> {
    let effect = EFFECTS.into_iter().find(|effect| effect.name() == name);
    if effect.is_none() && name != PANEL && name != DARK {
        tracing::warn!("cannot start {name:?}: there is no such effect");
        return Err(format!("there is no effect called {name:?}"));
    }
    let source = source.filter(|s| !s.is_empty()).map(String::from);

    halt(running);
    if retry {
        tracing::debug!("retrying {name}");
    } else {
        tracing::info!(source = source.as_deref(), "starting {name}");
    }
    // A press or marquee meant for the session just stopped is not one for this.
    lock(&MOODS).pressed = None;
    *lock(&MARQUEE) = None;

    let stop = Arc::new(AtomicBool::new(false));
    let dark = name == DARK;
    let wanted = (name.to_string(), source.clone());
    let showing = Arc::new(Mutex::new(Status::Running(match effect {
        Some(effect) => effect.name().to_string(),
        None => name.to_string(),
    })));
    let (flag, shown) = (Arc::clone(&stop), Arc::clone(&showing));
    // The session logs each of these itself.
    let report = move |report| match report {
        Report::Showing(effect) => *lock(&shown) = Status::Running(effect.name().to_string()),
        heard => {
            if let Report::Mood(name) = &heard {
                *lock(&shown) = Status::Mood(name.clone());
            }
            // Nobody listening, or the listener gone, is not the session's problem.
            if let Some(listener) = lock(&LISTENER).as_ref() {
                let _ = listener.send(heard);
            }
        }
    };
    let session = name.to_string();
    let thread = std::thread::Builder::new()
        .name(format!("effect: {name}"))
        .spawn(move || {
            let ended = match effect {
                Some(effect) => flash_dance_effects::run(
                    effect,
                    source.as_deref(),
                    &MOODS,
                    &MARQUEE,
                    &flag,
                    report,
                ),
                None if dark => flash_dance_effects::dark(&MOODS, &MARQUEE, &flag, report),
                None => flash_dance_effects::panel(&PICTURE, &MOODS, &MARQUEE, &flag, report),
            }
            .map_err(|e| e.to_string());
            // Logged here rather than when `state` collects it, so it is on time
            // and once, whoever ends up asking.
            match &ended {
                Ok(()) if flag.load(Ordering::Relaxed) => tracing::info!("{session} stopped"),
                Ok(()) => tracing::info!("{session} ended on the device"),
                Err(error) => {
                    let mut failure = lock(&FAILURE);
                    // The supervisor retries every second while the Launchpad is
                    // missing; only the first of a run of the same error is news.
                    if failure.as_ref().is_some_and(|(_, seen)| seen == error) {
                        tracing::debug!("{session} failed again: {error}");
                    } else {
                        tracing::error!("{session} failed: {error}");
                    }
                    *failure = Some((session.clone(), error.clone()));
                }
            }
            ended
        })
        .map_err(|e| {
            tracing::error!("cannot start a thread for {name}: {e}");
            e.to_string()
        })?;
    *running = Some(Running {
        showing,
        stop,
        thread,
        started: Instant::now(),
        wanted,
    });
    Ok(())
}

/// Presses a mood as if on the device, starting a session on the first effect if
/// none is running: the session is what shows it, and what tells Home Assistant.
pub fn press_mood(name: &str, source: Option<&str>) -> Result<Status, String> {
    if !lock(&MOODS).list.iter().any(|s| s.name == name) {
        tracing::warn!("cannot press {name:?}: there is no such mood");
        return Err(format!("there is no mood called {name:?}"));
    }
    tracing::info!("pressing mood {name}");
    if !matches!(status(), Status::Running(_) | Status::Mood(_)) {
        start(EFFECTS[0].name(), source)?;
    }
    lock(&MOODS).pressed = Some(name.to_string());
    Ok(Status::Mood(name.to_string()))
}

/// Scrolls `text` across the grid once, then shows the effect named `then` — or,
/// without one, the panel. The running effect session shows it, keeping its speed
/// and brightness; with none running (idle, the panel) one starts, on `then` or
/// the first effect. Without `then` the panel session shows it, starting if needed.
pub fn marquee(text: &str, then: Option<&str>, source: Option<&str>) -> Result<Status, String> {
    if text.trim().is_empty() {
        return Err("there is no text to scroll".to_string());
    }
    let then = match then {
        Some(name) => Some(
            EFFECTS
                .into_iter()
                .find(|effect| effect.name() == name)
                .ok_or_else(|| format!("there is no effect called {name:?}"))?,
        ),
        None => None,
    };
    match then {
        // A mood is shown by an effect session too, but it may be the panel's.
        // Neither the panel nor the dark grid is an effect session to show it on.
        Some(_) if matches!(status(), Status::Running(name) if name != PANEL && name != DARK) => {}
        Some(effect) => {
            start(effect.name(), source)?;
        }
        None if matches!(status(), Status::Running(name) if name == PANEL) => {}
        None => {
            start(PANEL, source)?;
        }
    }
    tracing::info!(then = then.map(|e| e.name()), "marquee {text:?}");
    *lock(&MARQUEE) = Some(Marquee {
        text: text.to_string(),
        then,
    });
    Ok(status())
}

/// Every LED off, the device still held and its presses still reported: what
/// "off" means when the server always holds the device.
pub fn dark() -> Status {
    let mut running = running();
    // Like the panel, it cannot fail to be found, only to connect.
    let _ = start_on(&mut running, DARK, None, false);
    state(&running)
}

/// Back to the panel: what "stop" means when the server always holds the device.
pub fn stop() -> Status {
    let mut running = running();
    // The panel cannot fail to be found, only to connect, and that is the status.
    let _ = start_on(&mut running, PANEL, None, false);
    state(&running)
}

pub fn status() -> Status {
    state(&running())
}

/// Keeps a session on the device from now until [`shutdown`]: the panel at once,
/// and again whenever a session ends — held Stop on the device, a crash, a
/// Launchpad that is missing or was unplugged, which it retries every [`RETRY`].
pub fn supervise() {
    SUPERVISING.store(true, Ordering::Relaxed);
    let spawned = std::thread::Builder::new()
        .name("supervisor".to_string())
        .spawn(|| {
            loop {
                {
                    let mut running = running();
                    // Checked under the lock `shutdown` takes after clearing it, so
                    // nothing starts once shutdown has stopped the session.
                    if !SUPERVISING.load(Ordering::Relaxed) {
                        return;
                    }
                    look_after(&mut running);
                }
                std::thread::sleep(RETRY);
            }
        });
    if let Err(error) = spawned {
        tracing::error!("cannot start the supervisor: {error}");
    }
}

/// One look: a session still up for a while has connected, which ends any
/// failure; one that has ended is collected and the panel goes back on.
fn look_after(running: &mut Option<Running>) {
    if let Some(current) = running.as_ref()
        && !current.thread.is_finished()
    {
        if current.started.elapsed() >= RETRY && lock(&FAILURE).take().is_some() {
            tracing::info!("the Launchpad is back");
        }
        return;
    }
    // Ended: how is already logged by the session, except a crash. A failure —
    // no Launchpad, most often — tries the same again; one that ended on purpose
    // or crashed goes to the panel, and so does boot, where nothing ended.
    let (name, source) = match running
        .take()
        .map(|ended| (ended.wanted, ended.thread.join()))
    {
        Some((wanted, Ok(Err(_)))) => wanted,
        Some((_, Ok(Ok(())))) => {
            tracing::info!("back to the panel");
            (PANEL.to_string(), None)
        }
        Some((_, Err(_))) => {
            tracing::error!("the session crashed; back to the panel");
            *lock(&FAILURE) = Some(("the session".to_string(), "it crashed".to_string()));
            (PANEL.to_string(), None)
        }
        None => (PANEL.to_string(), None),
    };
    // A retry says so at debug in `start_on`.
    let retry = lock(&FAILURE).is_some();
    let _ = start_on(running, &name, source.as_deref(), retry);
}

/// Stops the supervisor, then the session, so its `Drop` releases the device and
/// nothing puts another on it. For the server's exit.
pub fn shutdown() {
    SUPERVISING.store(false, Ordering::Relaxed);
    halt(&mut running());
}

/// Sets the session's stop flag and waits for it to return, which takes at most a
/// frame. Waiting matters: returning is what runs the `Launchpad`'s `Drop`, which
/// clears the LEDs and hands the MIDI port back for the next effect to open.
fn halt(running: &mut Option<Running>) {
    if let Some(current) = running.take() {
        // One that already ended has said so itself.
        if !current.thread.is_finished() {
            match &*lock(&current.showing) {
                Status::Mood(name) => tracing::info!("stopping mood {name}"),
                Status::Running(name) => tracing::info!("stopping {name}"),
                Status::Idle | Status::Failed { .. } => {}
            }
        }
        current.stop.store(true, Ordering::Relaxed);
        // A panic has been printed by the panic hook; this says what it ended.
        if current.thread.join().is_err() {
            tracing::error!("the session crashed while stopping");
        }
    }
}

/// What is on the device: the session's, while one is up; otherwise the failure
/// the supervisor is retrying, if there is one. Idle is only ever brief — between
/// a session ending and the supervisor's next look — or after [`shutdown`].
fn state(running: &Option<Running>) -> Status {
    if let Some(current) = running
        && !current.thread.is_finished()
    {
        return lock(&current.showing).clone();
    }
    match lock(&FAILURE).clone() {
        Some((effect, error)) => Status::Failed { effect, error },
        None => Status::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_source_list_is_news_once_per_change() {
        let (down, other) = ("no pactl".to_string(), "pactl failed".to_string());
        let mut last = None;
        assert_eq!(news(&mut last, None), None);
        assert_eq!(
            news(&mut last, Some(&down)),
            Some(News::Failing(down.clone()))
        );
        assert_eq!(news(&mut last, Some(&down)), None, "the same error again");
        assert_eq!(
            news(&mut last, Some(&other)),
            Some(News::Failing(other.clone()))
        );
        assert_eq!(news(&mut last, None), Some(News::Recovered));
        assert_eq!(news(&mut last, None), None);
    }
}
