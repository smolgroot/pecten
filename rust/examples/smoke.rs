//! Headless smoke run over a directory of local ROMs.
//!
//! ```sh
//! cargo run --release --example smoke -- <dir> [frames] [--no-speed]
//! ```
//!
//! Every `.gba` file directly inside `<dir>` (sorted by name) runs for `frames` frames
//! (default 600) with a fixed input script, and one table row is printed per ROM. ROMs are
//! only read from the given path; nothing is written. The table is plain text so two runs can
//! be diffed; everything but `speed` is deterministic, and `--no-speed` omits that column.
//!
//! Columns:
//! - `error`: the first core error and its frame, or `-`.
//! - `changed`: `yes` when any rendered frame differs from the first one.
//! - `audio`: share of audio samples that are non-zero, over the frames that ran.
//! - `backup`: the detected save type.
//! - `speed`: emulated frames per second relative to real time (59.7275 fps); depends on the
//!   machine and build, so do not diff it.
//!
//! A row describes one scripted run and says nothing about whether a game is playable.

use pecten::{CYCLES_PER_FRAME, GbaCore};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

const DEFAULT_FRAMES: u32 = 600;
const KEY_A: u16 = 1 << 0;
const KEY_START: u16 = 1 << 3;
const CPU_HZ: f64 = 16_777_216.0;

/// Start is held for 4 frames at the start of every 60, and A for 4 frames from the middle.
/// Pure function of the frame number, so runs are repeatable.
fn keys_for_frame(frame: u32) -> u16 {
    match frame % 60 {
        0..=3 => KEY_START,
        30..=33 => KEY_A,
        _ => 0,
    }
}

struct Report {
    error: Option<(u32, String)>,
    changed: bool,
    audio_share: f64,
    backup: &'static str,
    speed: f64,
}

fn run_rom(rom: &[u8], frames: u32) -> Result<Report, String> {
    let mut core = GbaCore::new();
    core.load_rom(rom).map_err(|error| error.to_string())?;

    let mut first_frame_hash = None;
    let mut changed = false;
    let mut error = None;
    let mut samples = 0u64;
    let mut non_silent = 0u64;
    let mut frames_run = 0u32;

    let started = Instant::now();
    for frame in 0..frames {
        core.set_pressed_keys(keys_for_frame(frame));
        match core.step_frame() {
            Ok(pixels) => {
                let mut hasher = DefaultHasher::new();
                pixels.hash(&mut hasher);
                let hash = hasher.finish();
                if *first_frame_hash.get_or_insert(hash) != hash {
                    changed = true;
                }
                frames_run += 1;
            }
            Err(failure) => {
                error = Some((frame, failure.to_string()));
                break;
            }
        }
        let audio = core.memory.apu.take_samples();
        samples += audio.len() as u64;
        non_silent += audio.iter().filter(|&&sample| sample != 0).count() as u64;
    }
    let elapsed = started.elapsed().as_secs_f64();

    let emulated_seconds = f64::from(frames_run) * f64::from(CYCLES_PER_FRAME) / CPU_HZ;
    Ok(Report {
        error,
        changed,
        audio_share: if samples == 0 {
            0.0
        } else {
            non_silent as f64 / samples as f64
        },
        backup: core.memory.backup.kind().name(),
        speed: if elapsed > 0.0 {
            emulated_seconds / elapsed
        } else {
            0.0
        },
    })
}

fn main() -> ExitCode {
    let mut dir = None;
    let mut frames = DEFAULT_FRAMES;
    let mut show_speed = true;
    for argument in std::env::args().skip(1) {
        if argument == "--no-speed" {
            show_speed = false;
        } else if dir.is_none() {
            dir = Some(PathBuf::from(argument));
        } else if let Ok(count) = argument.parse() {
            frames = count;
        } else {
            eprintln!("unexpected argument: {argument}");
            return ExitCode::from(2);
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: smoke <dir> [frames] [--no-speed]");
        return ExitCode::from(2);
    };

    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("cannot read {}: {error}", dir.display());
            return ExitCode::from(2);
        }
    };
    let mut roms: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("gba"))
        })
        .collect();
    roms.sort();
    if roms.is_empty() {
        eprintln!("no .gba files in {}", dir.display());
        return ExitCode::from(2);
    }

    let name_width = roms
        .iter()
        .map(|path| file_name(path).chars().count())
        .max()
        .unwrap_or(0)
        .max(3);
    let speed_header = if show_speed { "  speed" } else { "" };
    println!("frames: {frames}");
    println!(
        "{:<name_width$}  {:<7}  {:>5}  {:<8}{speed_header}  error",
        "rom", "changed", "audio", "backup"
    );

    let mut failures = 0;
    for path in &roms {
        let name = file_name(path);
        let outcome = std::fs::read(path)
            .map_err(|error| format!("read failed: {error}"))
            .and_then(|rom| run_rom(&rom, frames));
        match outcome {
            Ok(report) => {
                let speed = if show_speed {
                    format!("  {:>5.2}x", report.speed)
                } else {
                    String::new()
                };
                let error = match &report.error {
                    Some((frame, message)) => {
                        failures += 1;
                        format!("frame {frame}: {message}")
                    }
                    None => "-".to_owned(),
                };
                println!(
                    "{name:<name_width$}  {:<7}  {:>4.0}%  {:<8}{speed}  {error}",
                    if report.changed { "yes" } else { "no" },
                    report.audio_share * 100.0,
                    report.backup,
                );
            }
            Err(message) => {
                failures += 1;
                let speed = if show_speed { "      -" } else { "" };
                println!(
                    "{name:<name_width$}  {:<7}  {:>5}  {:<8}{speed}  load: {message}",
                    "-", "-", "-"
                );
            }
        }
    }
    println!("{} ROMs, {failures} with errors", roms.len());
    ExitCode::SUCCESS
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
