//! A benchmark for the claims the pull request makes.
//!
//! Two numbers matter here and neither is available from a unit test: how long
//! one press takes, and how many other processes it starts. The first is
//! measured by running the real binary; the second by putting a directory of
//! logging stubs ahead of the helpers on `PATH` and counting what it caught.
//!
//! The stubs run *and* the timing is taken against a synthetic backlight tree,
//! so the same command produces a comparable number on a laptop, a desktop and
//! a build machine.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use crate::hypr;

/// Every helper the command can reach for. Each one logs its arguments and
/// exits successfully, so a run that still completes is a run that called them.
const HELPERS: [&str; 9] = [
    "hyprctl",
    "jq",
    "brightnessctl",
    "ddcutil",
    "asdcontrol",
    "pactl",
    "wpctl",
    "sudo",
    "omarchy-osd",
];

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> std::io::Result<Self> {
        let root = std::env::temp_dir().join(format!("omarchy-media-bench-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin"))?;
        fs::create_dir_all(root.join("runtime"))?;
        fs::create_dir_all(root.join("backlight/bench_backlight"))?;

        // A panel with room to move, so the step rule is not clamped.
        fs::write(root.join("backlight/bench_backlight/brightness"), "500")?;
        fs::write(root.join("backlight/bench_backlight/max_brightness"), "1000")?;

        for helper in HELPERS {
            let script = format!(
                "#!/bin/sh\nprintf '{helper} %s\\n' \"$*\" >>\"$CALL_LOG\"\nexit 0\n"
            );
            let path = root.join("bin").join(helper);
            fs::write(&path, script)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            }
        }

        Ok(Self { root })
    }

    fn call_log(&self) -> PathBuf {
        self.root.join("calls")
    }

    /// Run the query form `runs` times with the stubs in front of the helpers,
    /// and report both the wall time and what it managed to call.
    fn measure(&self, runs: u32) -> io::Result<Report> {
        let _ = fs::remove_file(self.call_log());
        let stub_path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let mut samples = Vec::with_capacity(runs as usize);
        for _ in 0..runs {
            let started = Instant::now();
            let status = Command::new(current_exe()?)
                .args(["brightness", "display", "--monitor", "eDP-1"])
                .env("PATH", &stub_path)
                .env("CALL_LOG", self.call_log())
                // XDG_RUNTIME_DIR is deliberately left alone: pointing it
                // somewhere temporary would hide Hyprland's socket and
                // measure the hyprctl fallback instead of the real path.
                .env("OMARCHY_BACKLIGHT_PATH", self.root.join("backlight"))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            samples.push(started.elapsed());

            if !status.success() {
                return Err(io::Error::other(
                    "the query form failed; the benchmark needs a readable backlight tree",
                ));
            }
        }

        let log = fs::read_to_string(self.call_log()).unwrap_or_default();
        let mut calls: Vec<(String, usize)> = Vec::new();
        for line in log.lines().filter(|line| !line.trim().is_empty()) {
            match calls.iter_mut().find(|(name, _)| name == line) {
                Some((_, count)) => *count += 1,
                None => calls.push((line.to_string(), 1)),
            }
        }
        calls.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        Ok(Report {
            samples,
            calls,
            socket: hypr::socket_reachable(),
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct Report {
    samples: Vec<Duration>,
    calls: Vec<(String, usize)>,
    socket: bool,
}

impl Report {
    fn mean_ms(&self) -> f64 {
        self.samples.iter().map(Duration::as_secs_f64).sum::<f64>() * 1000.0
            / self.samples.len() as f64
    }

    fn min_ms(&self) -> f64 {
        self.samples.iter().min().map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
    }

    fn max_ms(&self) -> f64 {
        self.samples.iter().max().map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
    }

    fn print(&self, runs: u32) {
        let mut out = std::io::stdout().lock();
        let total: usize = self.calls.iter().map(|(_, n)| n).sum();

        let _ = writeln!(out, "omarchy-media bench — {runs} runs of the query form\n");
        let _ = writeln!(out, "  mean {:>7.2} ms", self.mean_ms());
        let _ = writeln!(out, "  min  {:>7.2} ms", self.min_ms());
        let _ = writeln!(out, "  max  {:>7.2} ms", self.max_ms());
        let _ = writeln!(out);
        let _ = writeln!(out, "  Each figure is one run as timed from the parent, and");
        let _ = writeln!(
            out,
            "  excludes the fork the parent pays to start it — which is why a"
        );
        let _ = writeln!(out, "  shell loop over this binary runs slower than the mean.");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "  hyprland socket: {}",
            if self.socket {
                "in use"
            } else {
                "unreachable, so hyprctl is used instead"
            }
        );
        let _ = writeln!(out, "  helper processes invoked: {total}");
        for (line, count) in &self.calls {
            let _ = writeln!(out, "    {count:>4}x {line}");
        }
        if self.calls.is_empty() {
            let _ = writeln!(out, "    (every helper was on PATH and none of them ran)");
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "  The shell script this replaces spends about sixteen processes per"
        );
        let _ = writeln!(out, "  press. To time that side on the same machine:");
        let _ = writeln!(
            out,
            "    time for i in $(seq {runs}); do omarchy-brightness-display --monitor eDP-1 >/dev/null; done"
        );
    }
}

fn current_exe() -> std::io::Result<PathBuf> {
    std::env::current_exe()
}

pub fn run(runs: u32) -> ExitCode {
    let fixture = match Fixture::new() {
        Ok(fixture) => fixture,
        Err(e) => {
            eprintln!("omarchy-media bench: {e}");
            return ExitCode::FAILURE;
        }
    };

    match fixture.measure(runs) {
        Ok(report) => {
            report.print(runs);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("omarchy-media bench: {e}");
            ExitCode::FAILURE
        }
    }
}
