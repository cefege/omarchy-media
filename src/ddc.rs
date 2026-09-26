//! External displays over DDC/CI, the path `omarchy-brightness-display-ddc`
//! takes. The I2C traffic still goes through `ddcutil`; what disappears is the
//! bash, the `awk` passes, and the `date` calls around it.
//!
//! The cache file is deliberately the same file, in the same format, with the
//! same expiry windows: the shell and this binary can take turns reading it
//! during a transition without a stale entry from one confusing the other.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const UNAVAILABLE_CACHE_SECONDS: u64 = 60;
const RANGE_CACHE_SECONDS: u64 = 10;

fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from("/tmp"),
    }
}

fn cache_file(monitor: &str) -> PathBuf {
    let sanitized: String = monitor
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    runtime_dir()
        .join("omarchy-brightness-display-ddc")
        .join(format!("{sanitized}.bus"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `(current * 100 + max / 2) / max` in integer arithmetic, the rounding the
/// shell's DDC branch uses.
pub fn percent_of(current: u64, max: u64) -> i64 {
    if max == 0 {
        return 0;
    }
    ((current * 100 + max / 2) / max) as i64
}

/// `(target * max + 50) / 100`, the inverse the shell uses before `setvcp`.
pub fn raw_of(target: i64, max: u64) -> u64 {
    (target.clamp(1, 100) as u64 * max + 50) / 100
}

struct Cache {
    bus: String,
    max: Option<u64>,
    at: Option<u64>,
}

impl Cache {
    fn read(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        let mut fields = text.split_whitespace();
        Some(Self {
            bus: fields.next()?.to_string(),
            max: fields.next().and_then(|f| f.parse().ok()),
            at: fields.next().and_then(|f| f.parse().ok()),
        })
    }

    /// A usable bus is a plain number; the shell checks the same with a regex.
    fn bus_number(&self) -> Option<&str> {
        (!self.bus.is_empty() && self.bus.chars().all(|c| c.is_ascii_digit())).then_some(&self.bus)
    }

    fn range_is_fresh(&self) -> bool {
        match (self.max, self.at) {
            (Some(max), Some(at)) if max > 0 => {
                let now = now();
                at <= now && now - at < RANGE_CACHE_SECONDS
            }
            _ => false,
        }
    }
}

fn write_cache(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, contents);
}

fn ddcutil(args: &[&str]) -> Option<String> {
    let output = Command::new("ddcutil")
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Pair each `I2C bus:` line with the `DRM connector:` line that follows it,
/// the way the awk filter in the shell script does.
fn detect_bus(monitor: &str, output: &str) -> Option<String> {
    let mut bus = String::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("I2C bus:") {
            let last = rest.split_whitespace().last().unwrap_or("");
            bus = match last.rfind("/i2c-") {
                Some(at) => last[at + "/i2c-".len()..].to_string(),
                None => last.to_string(),
            };
        } else if let Some(rest) = trimmed.strip_prefix("DRM connector:") {
            let last = rest.split_whitespace().last().unwrap_or("").to_string();
            let connector = strip_card_prefix(&last);
            if connector == monitor && !bus.is_empty() {
                return Some(bus);
            }
            bus.clear();
        }
    }
    None
}

fn strip_card_prefix(name: &str) -> String {
    match name.find('-') {
        Some(at)
            if name[..at].starts_with("card")
                && name[..at].len() > 4
                && name[4..at].chars().all(|c| c.is_ascii_digit()) =>
        {
            name[at + 1..].to_string()
        }
        _ => name.to_string(),
    }
}

/// The shell checks a detected bus with `^[0-9]+$` before it is written to the
/// cache or handed to ddcutil.
fn usable_bus(bus: &str) -> bool {
    !bus.is_empty() && bus.chars().all(|c| c.is_ascii_digit())
}

fn find_bus(monitor: &str, path: &Path) -> Option<String> {
    let mut bus = String::new();
    if let Some(cache) = Cache::read(path) {
        if cache.bus == "unavailable" {
            // Detection failed recently; do not pay for it on every key repeat.
            if now().saturating_sub(cache.at.unwrap_or(0)) < UNAVAILABLE_CACHE_SECONDS {
                return None;
            }
            let _ = fs::remove_file(path);
        } else {
            bus = cache.bus;
        }
    }

    if bus.is_empty() {
        let output = ddcutil(&["--skip-ddc-checks", "detect", "--brief"])?;
        match detect_bus(monitor, &output).filter(|bus| usable_bus(bus)) {
            Some(found) => {
                write_cache(path, &format!("{found}\n"));
                Some(found)
            }
            None => {
                write_cache(path, &format!("unavailable {}\n", now()));
                None
            }
        }
    } else {
        Some(bus)
    }
}

/// The current VCP 10 value and its maximum, from the same `getvcp` line the
/// awk filter read.
fn read_vcp(bus: &str) -> Option<(u64, u64)> {
    let output = ddcutil(&["--bus", bus, "--skip-ddc-checks", "getvcp", "10", "--brief"])?;
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 || fields[0] != "VCP" || !fields[1].eq_ignore_ascii_case("10") || fields[2] != "C" {
            continue;
        }
        let (Ok(current), Ok(max)) = (fields[3].parse::<u64>(), fields[4].parse::<u64>()) else {
            continue;
        };
        if max > 0 {
            return Some((current, max));
        }
    }
    None
}

pub struct Brightness {
    monitor: String,
    bus: String,
    current: u64,
    max: u64,
}

impl Brightness {
    pub fn read(monitor: &str) -> Option<Self> {
        let path = cache_file(monitor);
        let bus = find_bus(monitor, &path)?;
        let Some((current, max)) = read_vcp(&bus) else {
            // The monitor moved to another bus, or went away: drop the cache so
            // the next press redetects.
            let _ = fs::remove_file(&path);
            return None;
        };
        write_cache(&path, &format!("{bus} {current} {max} {}\n", now()));
        Some(Self {
            monitor: monitor.to_string(),
            bus,
            current,
            max,
        })
    }

    pub fn percent(&self) -> i64 {
        percent_of(self.current, self.max)
    }

    fn set(&self, target: i64) -> Result<(), String> {
        let raw = raw_of(target, self.max).to_string();
        let applied = ddcutil(&[
            "--bus",
            &self.bus,
            "--skip-ddc-checks",
            "--noverify",
            "setvcp",
            "10",
            &raw,
        ]);
        if applied.is_some() {
            Ok(())
        } else {
            // The bus may have moved; the shell drops the monitor's cache file
            // here so the next press redetects rather than reusing a bus that
            // just refused a write.
            let _ = fs::remove_file(cache_file(&self.monitor));
            Err("ddcutil setvcp failed".into())
        }
    }
}

/// The percentage the shell prints for an external monitor, or `None` when the
/// monitor cannot be reached.
pub fn query(monitor: &str) -> Option<i64> {
    Brightness::read(monitor).map(|b| b.percent())
}

/// Apply a step to an external monitor and return the percentage reached, which
/// is what the OSD shows. An absolute step may be served from the cache when
/// the range was read recently, exactly as the script's ten-second window
/// allows; a relative step always reads, because it needs the current value.
pub fn apply(monitor: &str, step: &str) -> Result<i64, String> {
    let path = cache_file(monitor);

    let target = if let Some(amount) = step.strip_prefix('+').and_then(|s| s.strip_suffix('%')) {
        let amount: i64 = amount.parse().map_err(|_| format!("invalid step: {step}"))?;
        let percent = Brightness::read(monitor).ok_or("could not read the monitor")?.percent();
        if amount == 5 && percent < 5 {
            percent + 1
        } else {
            percent + amount
        }
    } else if let Some(amount) = step.strip_suffix("%-") {
        let amount: i64 = amount.parse().map_err(|_| format!("invalid step: {step}"))?;
        let percent = Brightness::read(monitor).ok_or("could not read the monitor")?.percent();
        if amount == 5 && percent <= 5 {
            percent - 1
        } else {
            percent - amount
        }
    } else if let Some(absolute) = step.strip_suffix('%') {
        let target: i64 = absolute.parse().map_err(|_| format!("invalid step: {step}"))?;
        let fresh = Cache::read(&path)
            .is_some_and(|cache| cache.bus_number().is_some() && cache.range_is_fresh());
        if !fresh && Brightness::read(monitor).is_none() {
            return Err("could not read the monitor".into());
        }
        target
    } else {
        return Err(format!("invalid step: {step}"));
    };

    let target = target.clamp(1, 100);

    // Reuse the bus and range the read above just cached, rather than paying
    // for a second getvcp on the same press.
    let brightness = Brightness::read(monitor).ok_or("could not read the monitor")?;
    brightness.set(target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DETECT: &str = "\
I2C bus:      /dev/i2c-9
	Model:   Antec
DRM connector: eDP-1
	Model:   built-in
I2C bus:      /dev/i2c-12
	Model:   DELL U2720Q
DRM connector: DP-2
	Model:   DELL U2720Q
I2C bus:      /dev/i2c-14
	Model:   Acme
DRM connector: HDMI-1
";

    #[test]
    fn pairs_each_bus_with_the_connector_below_it() {
        assert_eq!(detect_bus("eDP-1", DETECT).as_deref(), Some("9"));
        assert_eq!(detect_bus("DP-2", DETECT).as_deref(), Some("12"));
        assert_eq!(detect_bus("HDMI-1", DETECT).as_deref(), Some("14"));
        assert_eq!(detect_bus("DP-3", DETECT), None);
    }

    #[test]
    fn strips_the_card_prefix_from_connector_names() {
        let output = "I2C bus: /dev/i2c-3\nDRM connector: card0-eDP-1\n";
        assert_eq!(detect_bus("eDP-1", output).as_deref(), Some("3"));
    }

    #[test]
    fn a_bus_without_a_path_is_kept_verbatim_and_then_rejected() {
        // ddcutil reports /dev/i2c-N, which the awk in the shell script
        // reduces to N. An older, pathless "9-0010" line is passed through
        // unchanged by that same filter, and then fails the shell's numeric
        // check, so the monitor is treated as unavailable rather than probed
        // with a nonsense bus number.
        assert_eq!(
            detect_bus("eDP-1", "I2C bus: 9-0010\nDRM connector: eDP-1\n").as_deref(),
            Some("9-0010")
        );
        assert!(!usable_bus("9-0010"));
        assert!(usable_bus("9"));
    }

    #[test]
    fn percent_uses_integer_round_half_up() {
        assert_eq!(percent_of(1, 3), 33);
        assert_eq!(percent_of(2, 3), 67);
        assert_eq!(percent_of(126, 420), 30);
        assert_eq!(percent_of(0, 420), 0);
        assert_eq!(percent_of(420, 420), 100);
        assert_eq!(percent_of(5, 0), 0);
    }

    #[test]
    fn raw_values_round_half_up_and_stay_in_range() {
        assert_eq!(raw_of(30, 420), 126);
        assert_eq!(raw_of(100, 420), 420);
        assert_eq!(raw_of(0, 420), 4);
        assert_eq!(raw_of(-5, 420), 4);
        assert_eq!(raw_of(150, 420), 420);
    }
}
