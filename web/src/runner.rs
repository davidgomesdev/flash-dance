//! Running effects, which means running the `effects` binary.
//!
//! The web server owns no rendering code of its own: every effect is the binary
//! you would run from a terminal, started with the name as its argument. The
//! binary is the API, `--list` included, so the site cannot drift out of step with
//! what the device can actually do.

use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};

use crate::app::{EffectInfo, SourceInfo, Status};

/// The one child. The Launchpad has a single MIDI port and the binary holds it
/// open, so exactly one effect can run at a time — starting one stops the last.
static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

struct Running {
    effect: String,
    child: Child,
    /// Held open only so that dropping it closes the child's stdin, which is how
    /// the binary is asked to quit — see [`kill`].
    stdin: Option<std::process::ChildStdin>,
}

fn running() -> MutexGuard<'static, Option<Running>> {
    // A panic while holding this lock would leave a child we still want to kill.
    RUNNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The `effects` binary: beside this server, where cargo puts both, and otherwise
/// whatever is on `PATH`.
fn binary() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("effects")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| "effects".into())
}

/// Every effect the binary reports, in its own order.
pub fn effects() -> io::Result<Vec<EffectInfo>> {
    let out = Command::new(binary()).arg("--list").output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "`effects --list` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let listed = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = listed.lines().collect();
    Ok(lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let (group, name) = line.split_once('\t')?;
            Some(EffectInfo {
                group: group.to_string(),
                name: name.to_string(),
                // The signature hue the device's own picker gives this effect, so
                // a card here is the colour of the pad there.
                hue: i as f32 / lines.len() as f32,
            })
        })
        .collect())
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

/// Starts an effect, stopping whatever was running first. `source` is the audio
/// source for the sound effects; the others ignore it.
pub fn start(effect: &str, source: Option<&str>) -> io::Result<Status> {
    let mut running = running();
    kill(&mut running);

    let mut command = Command::new(binary());
    command
        .arg(effect)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(source) = source.filter(|source| !source.is_empty()) {
        command.env("FLASH_DANCE_SOURCE", source);
    }
    let mut child = command.spawn()?;
    let stdin = child.stdin.take();
    *running = Some(Running {
        effect: effect.to_string(),
        child,
        stdin,
    });
    Ok(state(&mut running))
}

pub fn stop() -> Status {
    kill(&mut running());
    Status::Idle
}

pub fn status() -> Status {
    state(&mut running())
}

/// Asks the effect to quit by **closing its stdin**, and only insists if it will
/// not go.
///
/// Signals are the obvious thing and the wrong one: a Rust process takes the
/// default SIGTERM, which ends it without running `Drop`, and the binary's `Drop`
/// is what clears the LEDs and returns the device to standalone mode. Closing the
/// pipe instead lets it break its loop and exit the ordinary way. It also means an
/// effect cannot outlive this server — losing the write end is the same signal, so
/// killing the server tidies the device up rather than leaving it lit.
fn kill(running: &mut Option<Running>) {
    let Some(mut current) = running.take() else {
        return;
    };
    drop(current.stdin.take());
    for _ in 0..50 {
        match current.child.try_wait() {
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => return,
        }
    }
    // A second of not going is a hang; take the device being left lit over
    // wedging the request that asked for this.
    let _ = current.child.kill();
    let _ = current.child.wait();
}

/// What the child is doing, reaping it if it has finished. Exit 0 is the device's
/// own Stop/Solo/Mute; anything else is worth putting on the page, so the binary's
/// stderr — "no Launchpad Mini MK3 found", most likely — comes back with it.
fn state(running: &mut Option<Running>) -> Status {
    let Some(current) = running.as_mut() else {
        return Status::Idle;
    };
    match current.child.try_wait() {
        Ok(None) => Status::Running(current.effect.clone()),
        Ok(Some(exit)) => {
            let effect = current.effect.clone();
            // Only ever read at EOF: reading a live child's stderr would block the
            // request, and the binary's few lines fit the pipe buffer regardless.
            let mut said = String::new();
            if let Some(stderr) = current.child.stderr.as_mut() {
                let _ = stderr.read_to_string(&mut said);
            }
            *running = None;
            if exit.success() {
                Status::Idle
            } else {
                Status::Failed {
                    effect,
                    error: complaint(&said),
                }
            }
        }
        Err(e) => {
            *running = None;
            Status::Failed {
                effect: String::new(),
                error: e.to_string(),
            }
        }
    }
}

/// The one line worth showing out of whatever the binary printed while dying.
///
/// A returned `Err` from its `main` prints as `Error: …` and is the real cause, so
/// prefer that; it can be preceded by warnings that are not fatal ("no sound: …")
/// and followed by an unhelpful tail — a rejected effect name prints the whole
/// list of valid ones, whose last line is just another effect.
fn complaint(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines
        .iter()
        .find(|line| line.starts_with("Error: "))
        .or(lines.first())
        .unwrap_or(&"the effect stopped without saying why")
        .trim_start_matches("Error: ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_complaint_is_the_cause_not_the_last_thing_said() {
        // A rejected name prints the list of valid ones after the reason.
        let rejected =
            "no effect matches \"nonsense\". try one of:\n  rainbow diagonal\n  sound fire\n";
        assert_eq!(
            complaint(rejected),
            "no effect matches \"nonsense\". try one of:"
        );

        // A warning can come first and not be the reason it died.
        let unplugged =
            "no sound: no audio sources\nError: no Launchpad Mini MK3 found — is it plugged in?\n";
        assert_eq!(
            complaint(unplugged),
            "no Launchpad Mini MK3 found — is it plugged in?"
        );

        assert_eq!(complaint("  \n\n"), "the effect stopped without saying why");
    }
}
