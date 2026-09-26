//! End-to-end tests for the command, driven the way a desktop drives it.
//!
//! These replace the shell test suite that came with `bin/omarchy-brightness-display`,
//! case for case. The mocks are the same idea: a directory of stub `hyprctl`,
//! `ddcutil` and `brightnessctl` that log their arguments and answer with
//! percentages on stdout, the exact `ddcutil` calls, and when detection is and
//! is not repeated.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Harness {
    root: PathBuf,
}

impl Harness {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "omarchy-media-cli-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in ["bin", "runtime", "backlight"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }

        // A monitor list, so the binary gets a name without a compositor.
        write_script(
            &root.join("bin/hyprctl"),
            r#"#!/bin/sh
printf 'hyprctl %s\n' "$*" >>"$CALL_LOG"
case "$*" in
  *"-j"*) printf '[{"id":0,"name":"%s","make":"LENOVO","model":"21N9","focused":true,"disabled":false,"dpmsStatus":true}]\n' "${FOCUSED_MONITOR:-eDP-1}" ;;
esac
exit 0
"#,
        );

        // A DDC monitor on bus 7 wired to connector DP-1, with a VCP 10 range
        // the test can change through the environment.
        write_script(
            &root.join("bin/ddcutil"),
            r#"#!/bin/sh
printf 'ddcutil %s\n' "$*" >>"$CALL_LOG"
case "$*" in
  *detect*--brief*)
    printf 'Display 1\n   I2C bus:             /dev/i2c-%s\n   DRM connector:       card1-%s\n' \
      "${DDC_BUS:-7}" "${DDC_CONNECTOR:-DP-1}" ;;
  *getvcp*10*)
    [ "${DDC_READ_FAIL:-0}" = "1" ] && exit 1
    printf 'VCP 10 C %s %s\n' "${DDC_CURRENT:-40}" "${DDC_MAXIMUM:-80}" ;;
esac
exit 0
"#,
        );

        // Present so the internal-panel path can fall back to it if the kernel
        // node is not writable, which is what happens on most machines.
        write_script(
            &root.join("bin/brightnessctl"),
            r#"#!/bin/sh
printf 'brightnessctl %s\n' "$*" >>"$CALL_LOG"
exit 0
"#,
        );

        let harness = Self { root };
        harness.panel("mock_backlight", 40, 100);
        harness
    }

    fn panel(&self, device: &str, current: u64, max: u64) {
        let dir = self.root.join("backlight").join(device);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("brightness"), current.to_string()).unwrap();
        fs::write(dir.join("max_brightness"), max.to_string()).unwrap();
    }
    fn panel_value(&self, device: &str) -> u64 {
        fs::read_to_string(self.root.join("backlight").join(device).join("brightness"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }


    /// Per-invocation environment. The mocks read their answers from the
    /// child's environment, and the tests run in parallel threads of one
    /// process, so nothing here may touch this process's own environment.
    fn run(&self, args: &[&str]) -> Output {
        self.run_with(&[], args)
    }

    fn run_with(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_omarchy-media"));
        command
            .args(["brightness", "display"])
            .args(args)
            .env("PATH", path)
            .env("CALL_LOG", self.root.join("calls"))
            .env("XDG_RUNTIME_DIR", self.root.join("runtime"))
            .env("OMARCHY_BACKLIGHT_PATH", self.root.join("backlight"))
            .env_remove("FOCUSED_MONITOR")
            .env_remove("DDC_CURRENT")
            .env_remove("DDC_MAXIMUM")
            .env_remove("DDC_READ_FAIL")
            .env_remove("DDC_CONNECTOR")
            .env_remove("DDC_BUS");
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }


    fn calls(&self) -> String {
        fs::read_to_string(self.root.join("calls")).unwrap_or_default()
    }

    fn count(&self, needle: &str) -> usize {
        self.calls().lines().filter(|line| line.contains(needle)).count()
    }

    fn cache(&self, monitor: &str) -> PathBuf {
        self.root
            .join("runtime/omarchy-brightness-display-ddc")
            .join(format!("{monitor}.bus"))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn external_brightness_becomes_a_percentage() {
    let harness = Harness::new("query");
    let output = harness.run(&["--monitor", "DP-1"]);

    assert!(output.status.success(), "{:?}", output);
    // (40 * 100 + 40) / 80
    assert_eq!(stdout_of(&output), "50");
}

#[test]
fn the_ddc_bus_is_detected_once_and_then_cached() {
    let harness = Harness::new("detect-cache");
    harness.run(&["--monitor", "DP-1"]);
    assert_eq!(harness.count("detect --brief"), 1);

    harness.run(&["--monitor", "DP-1"]);
    assert_eq!(harness.count("detect --brief"), 1, "a cached bus is reused");
}

#[test]
fn an_external_percentage_is_converted_to_the_monitor_range() {
    let harness = Harness::new("absolute");
    harness.run(&["--no-osd", "--monitor", "DP-1", "25%"]);

    assert!(
        harness
            .calls()
            .contains("ddcutil --bus 7 --skip-ddc-checks --noverify setvcp 10 20"),
        "{}",
        harness.calls()
    );
}

#[test]
fn an_absolute_step_reuses_the_cached_range() {
    let harness = Harness::new("range-cache");
    harness.run(&["--monitor", "DP-1"]);

    let before = harness.count("getvcp 10");
    harness.run(&["--no-osd", "--monitor", "DP-1", "30%"]);

    // The range was read moments ago, so an absolute step spends no I2C
    // transaction at all; a held key would otherwise turn into a burst.
    assert_eq!(harness.count("getvcp 10"), before, "no read for a cached range");
    assert!(harness
        .calls()
        .contains("ddcutil --bus 7 --skip-ddc-checks --noverify setvcp 10 24"));
}

#[test]
fn the_internal_panel_is_read_from_the_kernel() {
    let harness = Harness::new("internal");
    let output = harness.run(&["--monitor", "eDP-1"]);

    assert!(output.status.success(), "{:?}", output);
    assert_eq!(stdout_of(&output), "40");
    // The kernel tree is the source of truth now, so brightnessctl is only a
    // fallback for a write the kernel refuses.
    assert_eq!(harness.count("brightnessctl"), 0);
}

#[test]
fn brightness_follows_the_focused_monitor() {
    let harness = Harness::new("focused");

    let output = harness.run(&[]);
    assert_eq!(stdout_of(&output), "40", "the focused panel is internal");

    let output = harness.run_with(&[("FOCUSED_MONITOR", "DP-1")], &[]);
    assert_eq!(stdout_of(&output), "50", "the focused external monitor is DDC");
}

#[test]
fn an_unsupported_monitor_has_no_backend() {
    let harness = Harness::new("unsupported");

    assert!(!harness.run(&["--monitor", "DP-2"]).status.success());

    let detections = harness.count("detect --brief");
    assert!(!harness.run_with(&[("DDC_CONNECTOR", "DP-1")], &["--monitor", "DP-2"])
        .status
        .success());
    assert_eq!(
        harness.count("detect --brief"),
        detections,
        "a monitor that cannot be reached is not rediscovered on every press"
    );
    assert!(
        fs::read_to_string(harness.cache("DP-2"))
            .unwrap_or_default()
            .starts_with("unavailable "),
        "the negative result is cached"
    );
}

#[test]
fn a_transient_read_failure_is_retried_next_time() {
    let harness = Harness::new("read-fail");

    let output = harness.run_with(&[("DDC_READ_FAIL", "1")], &["--monitor", "DP-1"]);
    assert!(!output.status.success(), "a failed read is reported");

    let output = harness.run(&["--monitor", "DP-1"]);
    assert!(output.status.success(), "the next press recovers: {output:?}");
    assert_eq!(stdout_of(&output), "50");
}

#[test]
fn an_expired_range_is_refreshed() {
    let harness = Harness::new("expired-range");
    harness.run(&["--monitor", "DP-1"]);

    // Hand-write a cache entry from eleven seconds ago, as a previous press
    // would have left one after the key was held down.
    fs::create_dir_all(harness.cache("DP-1").parent().unwrap()).unwrap();
    // The cache holds bus, range and a timestamp. Eleven seconds ago is past
    // the ten-second window, so the range has to be read again.
    let stale = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 11;
    fs::write(harness.cache("DP-1"), format!("7 80 {stale}\n")).unwrap();

    let before = harness.count("getvcp 10");
    // The refreshed range is 0-100, so 50 percent is 50 raw.
    harness.run_with(&[("DDC_MAXIMUM", "100")], &["--no-osd", "--monitor", "DP-1", "50%"]);

    assert_eq!(harness.count("getvcp 10"), before + 1, "a stale range is re-read");
    assert!(harness
        .calls()
        .contains("ddcutil --bus 7 --skip-ddc-checks --noverify setvcp 10 50"));
}

#[test]
fn a_low_external_brightness_uses_a_one_percent_step() {
    let harness = Harness::new("low-step");

    let dim = [("DDC_CURRENT", "4"), ("DDC_MAXIMUM", "100")];
    let output = harness.run_with(&dim, &["--no-osd", "--monitor", "DP-1", "+5%"]);

    assert!(output.status.success(), "{output:?}");
    // 4/100 is 4 percent, where a five percent step would jump off the bottom
    // of the range, so the target is one percent up: 5 raw.
    assert!(
        harness
            .calls()
            .contains("ddcutil --bus 7 --skip-ddc-checks --noverify setvcp 10 5"),
        "{}",
        harness.calls()
    );
}

#[test]
fn the_internal_step_rule_narrows_near_the_bottom() {
    let harness = Harness::new("internal-step");
    harness.panel("mock_backlight", 4, 100);

    let output = harness.run(&["--no-osd", "+5%"]);
    assert!(output.status.success(), "{output:?}");
    // 4 percent, so +5% becomes +1%: 5 percent, which on a 0-100 range is 5.
    assert_eq!(harness.panel_value("mock_backlight"), 5);
    // The kernel node was writable, so no helper was needed for the write.
    assert_eq!(harness.count("brightnessctl"), 0);
}

#[test]
fn an_absolute_internal_step_passes_straight_through() {
    let harness = Harness::new("internal-absolute");
    harness.run(&["--no-osd", "50%"]);

    assert_eq!(harness.panel_value("mock_backlight"), 50);
}

#[test]
fn an_internal_write_falls_back_to_brightnessctl_when_the_kernel_refuses() {
    let harness = Harness::new("internal-denied");
    let node = harness.root.join("backlight/mock_backlight/brightness");
    fs::set_permissions(&node, fs::Permissions::from_mode(0o444)).unwrap();

    let output = harness.run(&["--no-osd", "50%"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        harness
            .calls()
            .contains("brightnessctl -d mock_backlight set 50"),
        "{}",
        harness.calls()
    );
}

#[test]
fn a_missing_internal_device_is_an_error() {
    let harness = Harness::new("no-device");
    fs::remove_dir_all(harness.root.join("backlight/mock_backlight")).unwrap();

    let output = harness.run(&["--monitor", "eDP-1"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no backlight device"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
