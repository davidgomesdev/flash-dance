//! What the machine can hear, as numbers an effect can draw: eight octave bands,
//! where the sound sits among them and a beat flag. None of it depends on how
//! loud the source is — only on which frequencies it is made of.
//!
//! Capture is a `parec` child process, so **any PulseAudio/PipeWire source works
//! the same way** — a microphone, a line input, or a `.monitor` source, which is
//! whatever the machine is playing. That is the whole reason for shelling out
//! instead of binding an audio library: system sound is just another source name
//! here, where a portable capture crate would give us microphones and stop.
//!
//! ```no_run
//! # fn main() -> flash_dance_audio::Result<()> {
//! let mut audio = flash_dance_audio::Audio::open(None)?; // default: system sound
//! let sound = audio.listen();
//! println!("{:.2} {:?} {}", sound.pitch, sound.bands, sound.beat);
//! # Ok(()) }
//! ```

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Samples per analysis window. At [`RATE`] that is 93 ms and 10.8 Hz per bin.
/// Shorter windows are tempting for a snappier response, but the lowest octave is
/// only 40 Hz wide: at 1024 it gets two bins and a bass note lands in the wrong
/// one. The window always ends on the newest sample, so a kick is still seen the
/// frame it arrives — it is just averaged with what came before it.
const WINDOW: usize = 2048;

/// Mono, and only as fast as we need: the top band ends at 10 kHz.
const RATE: u32 = 22050;

/// One band per grid column, each an octave above the last starting at [`BASE_HZ`]
/// — 40 Hz to 10 kHz, which is where music lives.
pub const BANDS: usize = 8;
const BASE_HZ: f32 = 40.0;

/// What one window of audio sounded like. Everything is `0.0..=1.0` and relative to
/// the window itself, so the same music gives the same numbers at any volume, from
/// a quiet microphone or a loud monitor alike.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sound {
    /// Where the sound sits in the 40 Hz – 10 kHz range: the energy-weighted mean
    /// octave, `0.0` all bass and `1.0` all treble.
    pub pitch: f32,
    /// Energy per octave band, `bands[0]` the lowest, as a share of the loudest
    /// band in the same window — the shape of the spectrum, not its size.
    pub bands: [f32; BANDS],
    /// Set for the single window on which a beat was detected.
    pub beat: bool,
}

impl Sound {
    /// The two lowest bands — kick drums and bass lines.
    pub fn bass(&self) -> f32 {
        (self.bands[0] + self.bands[1]) * 0.5
    }
}

/// A capture source as PulseAudio names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    /// A monitor of an output, i.e. what the machine is playing rather than what a
    /// microphone hears.
    pub monitor: bool,
}

/// A sound-server command, with the runtime directory filled in when the
/// environment is missing one.
fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    if let Some(dir) = runtime_dir() {
        cmd.env("XDG_RUNTIME_DIR", dir);
    }
    cmd
}

/// PulseAudio finds its socket under `XDG_RUNTIME_DIR`, and a process started
/// outside a login session — a systemd unit, a cron job, an agent's shell — often
/// has none. Then libpulse fails with "Connection refused", which sounds like a
/// dead sound server and is not one. The spec's own default for the variable is
/// `/run/user/<uid>`, so supply that rather than the misleading error.
///
/// `None` when the environment already has one, which is never overridden.
fn runtime_dir() -> Option<String> {
    if std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|dir| !dir.is_empty()) {
        return None;
    }
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self").ok()?.uid();
    let dir = format!("/run/user/{uid}");
    std::path::Path::new(&dir)
        .join("pulse")
        .is_dir()
        .then_some(dir)
}

/// Every source the sound server will let us record from.
pub fn sources() -> Result<Vec<Source>> {
    let out = command("pactl")
        .args(["list", "short", "sources"])
        .output()
        .map_err(|e| format!("could not run pactl ({e}) — install pulseaudio-utils"))?;
    if !out.status.success() {
        return Err(format!(
            "pactl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let name = line.split('\t').nth(1)?.to_string();
            Some(Source {
                monitor: name.ends_with(".monitor"),
                name,
            })
        })
        .collect())
}

/// A running capture. Dropping it stops the child process.
pub struct Audio {
    child: Child,
    source: String,
    window: Arc<Mutex<Ring>>,
    bass_avg: f32,
    last_beat: Instant,
    last: Instant,
}

impl Audio {
    /// Opens a source whose name contains `source`, or the default when `None`:
    /// the first monitor — system sound — falling back to the first input.
    pub fn open(source: Option<&str>) -> Result<Audio> {
        let sources = sources()?;
        let picked = match source {
            Some(want) => sources
                .iter()
                .find(|s| s.name.contains(want))
                .ok_or_else(|| {
                    format!(
                        "no source matching {want:?}. available:\n{}",
                        list(&sources)
                    )
                })?,
            None => sources
                .iter()
                .find(|s| s.monitor)
                .or_else(|| sources.first())
                .ok_or("no audio sources — is PipeWire or PulseAudio running?")?,
        };
        Audio::record(&picked.name)
    }

    fn record(source: &str) -> Result<Audio> {
        let mut child = command("parec")
            .args([
                "--format=s16le",
                "--channels=1",
                "--raw",
                // Without this parec hands over a second of audio at a time, and
                // the analysis freezes between deliveries instead of streaming.
                "--latency-msec=25",
                &format!("--rate={RATE}"),
                "-d",
                source,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not run parec ({e}) — install pulseaudio-utils"))?;

        let mut out = child.stdout.take().ok_or("parec gave us no stdout")?;
        let window = Arc::new(Mutex::new(Ring::new()));
        let sink = Arc::clone(&window);
        std::thread::spawn(move || {
            // Whole samples only: a short read that split one in half would shift
            // every byte after it and turn the stream into noise.
            let mut buf = [0u8; 512];
            while out.read_exact(&mut buf).is_ok() {
                let mut ring = sink.lock().unwrap_or_else(|e| e.into_inner());
                for sample in buf.chunks_exact(2) {
                    ring.push(i16::from_le_bytes([sample[0], sample[1]]) as f32 / 32768.0);
                }
            }
        });

        let now = Instant::now();
        Ok(Audio {
            child,
            source: source.to_string(),
            window,
            bass_avg: 0.0,
            last_beat: now,
            last: now,
        })
    }

    /// The source being recorded, as the sound server names it.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Analyses the most recent window. Call it once a frame — the beat average and
    /// the beat's refractory period advance from here.
    pub fn listen(&mut self) -> Sound {
        let now = Instant::now();
        // A long gap (the app was in its picker, say) must not flush the average.
        let dt = (now - self.last).as_secs_f32().min(0.5);
        self.last = now;

        let samples = self.window.lock().unwrap_or_else(|e| e.into_inner()).read();
        let Some(raw) = heard(&samples) else {
            return Sound::default();
        };

        // A ratio of the bass to its own recent past, so turning the source up or
        // down scales both sides and leaves the beats where they were.
        let bass = raw[0] + raw[1];
        let beat =
            bass > self.bass_avg * BEAT_RATIO && now.duration_since(self.last_beat) > BEAT_GAP;
        if beat {
            self.last_beat = now;
        }
        // Frame-rate independent EMA: the average is over a second of sound, not
        // over a fixed number of frames.
        self.bass_avg += (bass - self.bass_avg) * (1.0 - 0.1f32.powf(dt));

        Sound { beat, ..shape(raw) }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        // The reader thread ends on its own once the pipe closes.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Beat when the bass is this much above its running average.
const BEAT_RATIO: f32 = 1.5;
/// No two beats closer than this — one kick spans several windows.
const BEAT_GAP: Duration = Duration::from_millis(120);
/// RMS below which a window is silence, about -66 dBFS. The one place volume still
/// counts: without it, a room's hiss or a stream's dither would be scaled up like
/// anything else and drawn as a full spectrum. Raise it if a microphone in a quiet
/// room still lights the grid; lower it if a quiet source looks dead.
const SILENCE: f32 = 5e-4;

/// The raw band energies of a window, or `None` when it is silence.
fn heard(samples: &[f32; WINDOW]) -> Option<[f32; BANDS]> {
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / WINDOW as f32).sqrt();
    (rms >= SILENCE).then(|| bands_of(samples))
}

/// The volume-free part of a [`Sound`]: every band over the loudest, so scaling the
/// input scales both and cancels out.
fn shape(raw: [f32; BANDS]) -> Sound {
    let loudest = raw.iter().copied().fold(f32::MIN_POSITIVE, f32::max);
    let total: f32 = raw.iter().sum::<f32>().max(f32::MIN_POSITIVE);
    let centre: f32 = raw
        .iter()
        .enumerate()
        .map(|(i, e)| i as f32 * e)
        .sum::<f32>()
        / total;
    Sound {
        pitch: centre / (BANDS - 1) as f32,
        // Square root because the top of the range is where the eye stops noticing:
        // a linear bar spends most of its travel looking full.
        bands: raw.map(|band| (band / loudest).sqrt()),
        beat: false,
    }
}

/// Energy per octave band of one window of samples.
fn bands_of(samples: &[f32; WINDOW]) -> [f32; BANDS] {
    let mut re = *samples;
    let mut im = [0.0; WINDOW];
    // Hann window: a tone that does not fit the window a whole number of times
    // smears across every band without it.
    for (i, sample) in re.iter_mut().enumerate() {
        *sample *= 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / WINDOW as f32).cos();
    }
    fft(&mut re, &mut im);

    let mut bands = [0.0; BANDS];
    for (i, band) in bands.iter_mut().enumerate() {
        let lo = bin(BASE_HZ * (1 << i) as f32);
        let hi = bin(BASE_HZ * (1 << (i + 1)) as f32).min(WINDOW / 2);
        // Summed, not averaged: music loses roughly as much energy per octave as
        // an octave gains bins, so the sum is what comes out level across a bar.
        *band = (lo..hi)
            .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt())
            .sum();
    }
    bands
}

/// The FFT bin a frequency lands in.
fn bin(hz: f32) -> usize {
    (hz * WINDOW as f32 / RATE as f32) as usize
}

/// In-place radix-2 Cooley-Tukey. `re` and `im` must be the same power-of-two
/// length. Here rather than as a dependency because this is the whole of it, and
/// the crate has no others.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);

    // Bit-reversal permutation, so the butterflies below can run in place.
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2;
    while len <= n {
        let step = -std::f32::consts::TAU / len as f32;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (sin, cos) = (step * k as f32).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cos - im[b] * sin, re[b] * sin + im[b] * cos);
                (re[b], im[b]) = (re[a] - tr, im[a] - ti);
                (re[a], im[a]) = (re[a] + tr, im[a] + ti);
            }
        }
        len <<= 1;
    }
}

/// Source names, one per line, for an error message.
fn list(sources: &[Source]) -> String {
    sources
        .iter()
        .map(|s| {
            format!(
                "  {}{}",
                s.name,
                if s.monitor { "  (system sound)" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The newest [`WINDOW`] samples, oldest first.
struct Ring {
    buf: [f32; WINDOW],
    at: usize,
}

impl Ring {
    fn new() -> Ring {
        Ring {
            buf: [0.0; WINDOW],
            at: 0,
        }
    }

    fn push(&mut self, sample: f32) {
        self.buf[self.at] = sample;
        self.at = (self.at + 1) % WINDOW;
    }

    fn read(&self) -> [f32; WINDOW] {
        let mut out = [0.0; WINDOW];
        let (old, new) = self.buf.split_at(self.at);
        out[..new.len()].copy_from_slice(new);
        out[new.len()..].copy_from_slice(old);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine at `hz`, full scale.
    fn tone(hz: f32) -> [f32; WINDOW] {
        let mut samples = [0.0; WINDOW];
        for (i, sample) in samples.iter_mut().enumerate() {
            *sample = (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin();
        }
        samples
    }

    fn loudest(bands: [f32; BANDS]) -> usize {
        (0..BANDS)
            .max_by(|a, b| bands[*a].total_cmp(&bands[*b]))
            .unwrap()
    }

    #[test]
    fn fft_puts_a_tone_in_its_own_bin() {
        let mut re = tone(1000.0);
        let mut im = [0.0; WINDOW];
        fft(&mut re, &mut im);
        let power = |k: usize| re[k] * re[k] + im[k] * im[k];
        let peak = (0..WINDOW / 2)
            .max_by(|a, b| power(*a).total_cmp(&power(*b)))
            .unwrap();
        // Within a bin: `bin` truncates, and 1000 Hz falls between two of them.
        assert!(peak.abs_diff(bin(1000.0)) <= 1, "peaked at bin {peak}");
    }

    #[test]
    fn each_octave_lands_in_its_own_band() {
        // The middle of each band, which is 40 * 2^i to 40 * 2^(i+1).
        for i in 0..BANDS {
            let hz = BASE_HZ * (1 << i) as f32 * 1.5;
            assert_eq!(loudest(bands_of(&tone(hz))), i, "{hz} Hz");
        }
    }

    #[test]
    fn silence_is_silent() {
        assert_eq!(bands_of(&[0.0; WINDOW]), [0.0; BANDS]);
    }

    /// Two tones at once, a bass note and a treble one, at `amplitude`.
    fn chord(amplitude: f32) -> [f32; WINDOW] {
        let (low, high) = (tone(100.0), tone(3000.0));
        std::array::from_fn(|i| amplitude * (low[i] + 0.3 * high[i]) / 1.3)
    }

    #[test]
    fn volume_changes_nothing() {
        let loud = shape(heard(&chord(1.0)).unwrap());
        for amplitude in [0.3, 0.01, 0.002] {
            let quiet = shape(heard(&chord(amplitude)).unwrap());
            assert!(
                (quiet.pitch - loud.pitch).abs() < 1e-3,
                "{amplitude}: pitch {quiet:?}"
            );
            for (q, l) in quiet.bands.iter().zip(loud.bands) {
                assert!(
                    (q - l).abs() < 1e-3,
                    "{amplitude}: {:?} vs {:?}",
                    quiet.bands,
                    loud.bands
                );
            }
        }
    }

    #[test]
    fn pitch_rises_with_frequency() {
        let pitch = |hz| shape(bands_of(&tone(hz))).pitch;
        assert!(pitch(60.0) < 0.1 && pitch(8000.0) > 0.9);
        assert!(pitch(60.0) < pitch(500.0) && pitch(500.0) < pitch(8000.0));
    }

    #[test]
    fn silence_stays_dark() {
        assert_eq!(heard(&[0.0; WINDOW]), None);
        // Hiss at about -80 dBFS: quiet enough to be nothing.
        assert_eq!(heard(&tone(5000.0).map(|s| s * 1e-4)), None);
    }

    #[test]
    fn the_ring_reads_oldest_first() {
        let mut ring = Ring::new();
        for i in 0..WINDOW + 3 {
            ring.push(i as f32);
        }
        let read = ring.read();
        assert_eq!(read[0], 3.0);
        assert_eq!(read[WINDOW - 1], (WINDOW + 2) as f32);
    }
}
