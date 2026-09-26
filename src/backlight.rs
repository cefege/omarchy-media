//! The internal panel: `/sys/class/backlight`, read and written directly.
//!
//! `omarchy-hw-display` and `omarchy-brightness-display` reach this through
//! `brightnessctl`, once to find the current percentage and again to set it.
//! The kernel exposes the same two numbers, and the percentage rule is
//! brightnessctl's: `roundf(current / max * 100)` in `f32`, exponent 1. That
//! arithmetic is reproduced here exactly, because the OSD shows the number and
//! a rule that differs by a rounding step is a rule that differs on screen.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_BACKLIGHT_PATH: &str = "/sys/class/backlight";

/// A device the heuristic in `omarchy-hw-display` prefers when several exist.
/// The Touch Bar registers a backlight that never drives the panel, and on
/// dual-GPU Macs the panel follows one GPU, so a plain "first device" pick is
/// wrong often enough to matter.
const PREFERRED: [&str; 4] = ["gmux_backlight", "amdgpu_bl", "intel_backlight", "acpi_video"];
const EXCLUDED: &str = "appletb_backlight";

pub fn class_path() -> PathBuf {
    match std::env::var_os("OMARCHY_BACKLIGHT_PATH") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from(DEFAULT_BACKLIGHT_PATH),
    }
}

fn read_u64(path: &Path) -> Result<u64, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.trim()
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn device_dir(base: &Path, device: &str) -> PathBuf {
    base.join(device)
}

/// The device `omarchy-hw-display` would have printed, chosen the same way:
/// the first entry that is not a Touch Bar, refined by a heuristic order.
pub fn pick_device() -> Result<String, String> {
    pick_device_in(&class_path())
}

pub fn pick_device_in(base: &Path) -> Result<String, String> {
    let mut entries: Vec<String> = fs::read_dir(base)
        .map_err(|e| format!("{}: {e}", base.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();

    let mut device = entries
        .iter()
        .find(|name| name.as_str() != EXCLUDED)
        .cloned();

    for prefix in PREFERRED {
        if let Some(found) = entries.iter().find(|name| name.starts_with(prefix)) {
            device = Some(found.clone());
            break;
        }
    }

    device.ok_or_else(|| format!("no backlight device found in {}", base.display()))
}

pub struct Backlight {
    pub device: String,
    pub current: u64,
    pub max: u64,
}

impl Backlight {
    pub fn read(device: &str) -> Result<Self, String> {
        Self::read_in(&class_path(), device)
    }

    pub fn read_in(base: &Path, device: &str) -> Result<Self, String> {
        let dir = device_dir(base, device);
        Ok(Self {
            device: device.to_string(),
            current: read_u64(&dir.join("brightness"))?,
            max: read_u64(&dir.join("max_brightness"))?,
        })
    }

    /// brightnessctl's `val_to_percent`: `roundf(current / max * 100)`.
    pub fn percent(&self) -> i64 {
        if self.max == 0 {
            return 0;
        }
        let ratio = self.current as f32 / self.max as f32;
        (ratio * 100.0).round() as i64
    }

    /// brightnessctl's `percent_to_val`: `roundf(percent / 100 * max)`.
    pub fn value_for_percent(percent: i64, max: u64) -> u64 {
        let scaled = percent as f32 / 100.0;
        (scaled * max as f32).round().max(0.0) as u64
    }

    /// Set the panel to a percentage, the way `brightnessctl set N%` would.
    pub fn set_percent(&self, percent: i64) -> Result<(), String> {
        self.set_value(Self::value_for_percent(percent, self.max))
    }

    pub fn set_value(&self, value: u64) -> Result<(), String> {
        let path = device_dir(&class_path(), &self.device).join("brightness");
        match fs::write(&path, value.to_string()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                // The sysfs node is root-owned on most machines; brightnessctl
                // gets around that through systemd-logind, which is a
                // permission model, not an arithmetic one. Keep one child
                // process for the write rather than reimplementing login1's
                // D-Bus call.
                self.set_via_brightnessctl(value)
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn set_via_brightnessctl(&self, value: u64) -> Result<(), String> {
        let status = Command::new("brightnessctl")
            .args(["-d", &self.device, "set", &value.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("could not run brightnessctl: {e}"))?;

        if status.success() {
            Ok(())
        } else {
            Err(format!("could not set brightness on {}", self.device))
        }
    }
}

/// The step rule `omarchy-brightness-display` applies to `+5%` and `5%-`
/// before handing the result to brightnessctl: near the bottom of the range a
/// 5% jump is invisible, so a single percent is used instead. Absolute steps
/// are passed through untouched.
pub fn resolve_step(step: &str, current_percent: i64) -> String {
    match step {
        "+5%" => {
            let target = if current_percent < 5 {
                current_percent + 1
            } else {
                current_percent + 5
            };
            format!("{}%", target.min(100))
        }
        "5%-" => {
            let target = if current_percent <= 5 {
                current_percent - 1
            } else {
                current_percent - 5
            };
            format!("{}%", target.max(1))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fake_device(dir: &Path, name: &str, current: u64, max: u64) {
        let device = dir.join(name);
        fs::create_dir_all(&device).unwrap();
        fs::write(device.join("brightness"), current.to_string()).unwrap();
        fs::write(device.join("max_brightness"), max.to_string()).unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "omarchy-media-test-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn percent_matches_brightnessctl_rounding() {
        // brightnessctl -m on an apple-panel-bl reported 126,420 -> 30%.
        let panel = Backlight {
            device: "apple-panel-bl".into(),
            current: 126,
            max: 420,
        };
        assert_eq!(panel.percent(), 30);

        // The rounding rule is round-half-away-from-zero on an f32 quotient.
        for max in [1u64, 2, 3, 7, 420, 1000, 1400] {
            for current in 0..=max {
                let b = Backlight {
                    device: "d".into(),
                    current,
                    max,
                };
                let expected = (current as f32 / max as f32 * 100.0).round() as i64;
                assert_eq!(b.percent(), expected, "current={current} max={max}");
            }
        }
    }

    #[test]
    fn value_for_percent_is_the_inverse_rounding() {
        assert_eq!(Backlight::value_for_percent(30, 420), 126);
        assert_eq!(Backlight::value_for_percent(100, 420), 420);
        assert_eq!(Backlight::value_for_percent(0, 420), 0);
        assert_eq!(Backlight::value_for_percent(1, 420), 4);
    }

    #[test]
    fn reads_a_device_from_a_fake_tree() {
        let dir = temp_dir("read");
        fake_device(&dir, "intel_backlight", 500, 1000);
        let panel = Backlight::read_in(&dir, "intel_backlight").unwrap();
        assert_eq!(panel.percent(), 50);
    }

    #[test]
    fn device_choice_matches_the_omarchy_heuristic() {
        let dir = temp_dir("pick");
        // A Touch Bar alone is not a panel: the shell excludes it by name.
        fake_device(&dir, EXCLUDED, 10, 20);
        assert!(pick_device_in(&dir).is_err());

        // Otherwise the first non-Touch-Bar entry wins.
        fake_device(&dir, "zzz_backlight", 10, 20);
        fake_device(&dir, "aaa_backlight", 10, 20);
        assert_eq!(pick_device_in(&dir).unwrap(), "aaa_backlight");

        // A preferred driver takes precedence over sort order, and the first
        // matching prefix in the list wins among the preferred ones.
        fake_device(&dir, "gmux_backlight", 10, 20);
        assert_eq!(pick_device_in(&dir).unwrap(), "gmux_backlight");

        let amd = temp_dir("pick-amd");
        fake_device(&amd, "intel_backlight", 10, 20);
        fake_device(&amd, "amdgpu_bl0", 10, 20);
        fake_device(&amd, "amdgpu_bl1", 10, 20);
        assert_eq!(pick_device_in(&amd).unwrap(), "amdgpu_bl0");
    }

    #[test]
    fn step_rule_narrows_near_the_bottom() {
        assert_eq!(resolve_step("+5%", 0), "1%");
        assert_eq!(resolve_step("+5%", 4), "5%");
        assert_eq!(resolve_step("+5%", 5), "10%");
        assert_eq!(resolve_step("+5%", 96), "100%");
        assert_eq!(resolve_step("+5%", 98), "100%");
        assert_eq!(resolve_step("5%-", 0), "1%");
        assert_eq!(resolve_step("5%-", 5), "4%");
        assert_eq!(resolve_step("5%-", 6), "1%");
        assert_eq!(resolve_step("5%-", 40), "35%");
        // Absolute steps pass straight through, including the endpoints.
        assert_eq!(resolve_step("50%", 3), "50%");
        assert_eq!(resolve_step("100%", 3), "100%");
        assert_eq!(resolve_step("1%", 3), "1%");
    }
}
