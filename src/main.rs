//! Native replacements for the Omarchy shell commands on the input path.
//!
//! The program dispatches on the name it was invoked as, so the same binary
//! can be installed under the name of each command it replaces. That keeps the
//! Hyprland bindings, the menus, and anything a user has scripted against
//! `omarchy-brightness-display` working unchanged: a PR swaps which file
//! answers to that name, and nothing that calls it has to know.

mod apple;
mod backlight;
mod ddc;
mod hypr;
mod json;
mod osd;

use std::io::Write;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: omarchy-brightness-display [--no-osd] [--monitor <name>] [+N%|N%-|N%|off|on]

Show or adjust brightness on the focused display. Options and defaults match
omarchy-brightness-display, which this replaces.

Options:
      --no-osd            Set the brightness without showing the OSD
      --monitor <name>    Act on a named Hyprland monitor instead of the
                          focused one
  -h, --help              Show this help
";

/// The shell treats a leading eDP-, LVDS- or DSI- connector as the internal
/// panel, which is the one it can write directly.
fn is_internal_panel(monitor: &str) -> bool {
    ["eDP-", "LVDS-", "DSI-"]
        .iter()
        .any(|prefix| monitor.starts_with(prefix))
}

/// `flock -n` on the file the shell locks, so two key repeats racing each other
/// do not interleave a read, a step, and a write.
struct StepLock {
    _file: std::fs::File,
}

impl StepLock {
    /// `None` when another invocation already holds it, which the shell treats
    /// as "drop this key repeat and exit quietly".
    fn acquire() -> Option<Self> {
        use std::os::unix::io::AsRawFd;

        let runtime = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
            _ => std::path::PathBuf::from("/tmp"),
        };
        let path = runtime.join("omarchy-brightness-display.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .ok()?;

        // SAFETY: `file` owns a valid descriptor for the duration of the call.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        (locked == 0).then_some(Self { _file: file })
    }
}

fn brightness_display(args: &[String]) -> ExitCode {
    let mut no_osd = false;
    let mut monitor: Option<String> = None;
    let mut step: Option<&str> = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--no-osd" => no_osd = true,
            "--monitor" => {
                index += 1;
                match args.get(index) {
                    Some(name) => monitor = Some(name.clone()),
                    None => {
                        eprintln!("omarchy-brightness-display: --monitor needs a name");
                        return ExitCode::from(2);
                    }
                }
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                step = Some(other);
                break;
            }
        }
        index += 1;
    }

    // One socket round trip answers both questions the shell asks with two
    // separate `hyprctl monitors -j | jq` pipelines.
    let known_monitors = hypr::monitors().unwrap_or_default();
    let monitor = monitor.or_else(|| hypr::focused_in(&known_monitors));
    let apple = hypr::apple_for(&known_monitors, monitor.as_deref());
    let external = monitor.as_deref().is_some_and(|name| !is_internal_panel(name));

    let Some(step) = step else {
        // No step: print the current percentage, as the query form does.
        let percent = (|| -> Result<i64, String> {
            if apple {
                apple::query()
            } else if external {
                ddc::query(monitor.as_deref().unwrap_or_default())
                    .ok_or_else(|| "could not read the monitor".to_string())
            } else {
                let device = backlight::pick_device()?;
                backlight::Backlight::read(&device).map(|panel| panel.percent())
            }
        })();

        return match percent {
            Ok(percent) => {
                println!("{percent}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("omarchy-brightness-display: {e}");
                ExitCode::FAILURE
            }
        };
    };

    if step == "off" {
        return match hypr::dispatch_dpms(false) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("omarchy-brightness-display: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if step == "on" {
        // Skip the dispatch when every active display is already lit: a
        // redundant DPMS enable right after resume forces another modeset,
        // which blanks the panel for a beat at the unlock screen.
        if hypr::every_display_lit() {
            return ExitCode::SUCCESS;
        }
        return match hypr::dispatch_dpms(true) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("omarchy-brightness-display: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let Some(_lock) = StepLock::acquire() else {
        return ExitCode::SUCCESS;
    };

    let shown = if apple {
        apple::apply(step)
    } else if external {
        ddc::apply(monitor.as_deref().unwrap_or_default(), step)
    } else {
        apply_internal(step)
    };

    match shown {
        Ok(percent) => {
            if !no_osd {
                // Best effort, and quiet: a screensaver or menu that cannot
                // reach the shell should not turn a brightness key into an
                // error on screen.
                let _ = osd::brightness(percent, true);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("omarchy-brightness-display: {e}");
            ExitCode::FAILURE
        }
    }
}

fn apply_internal(step: &str) -> Result<i64, String> {
    let device = backlight::pick_device()?;
    let panel = backlight::Backlight::read(&device)?;
    let resolved = backlight::resolve_step(step, panel.percent());
    panel.set_percent(resolved.trim_end_matches('%').parse().map_err(|_| "invalid step".to_string())?)?;

    // The OSD shows the panel's real percentage after the write, not the
    // percentage that was asked for: the panel rounds.
    Ok(backlight::Backlight::read(&device)?.percent())
}

fn run(argv: Vec<String>) -> ExitCode {
    let program = argv
        .first()
        .map(|path| {
            std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let args: Vec<String> = argv.into_iter().skip(1).collect();

    match program.as_str() {
        "omarchy-brightness-display" => brightness_display(&args),
        "omarchy-media" => match args.first().map(String::as_str) {
            Some("brightness") if args.get(1).map(String::as_str) == Some("display") => {
                brightness_display(&args[2..])
            }
            _ => {
                let _ = writeln!(std::io::stderr(), "omarchy-media: unknown command");
                let _ = write!(std::io::stderr(), "{USAGE}");
                ExitCode::from(2)
            }
        },
        _ => {
            let _ = writeln!(
                std::io::stderr(),
                "omarchy-media: invoked as `{program}`, which is not a command it provides"
            );
            ExitCode::from(2)
        }
    }
}

fn main() -> ExitCode {
    run(std::env::args().collect())
}
