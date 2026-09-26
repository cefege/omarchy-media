//! Apple Studio and XDR displays, the path
//! `omarchy-brightness-display-apple` takes. `asdcontrol` needs root and a HID
//! device, so both stay; the bash, the `awk` field splitting, and the retry
//! plumbing around them do not.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BRIGHTNESS_CEILING: f64 = 60000.0;

fn device_cache() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    if runtime.is_empty() {
        return None;
    }
    Some(PathBuf::from(runtime).join("omarchy-brightness-display-apple.device"))
}

fn looks_like_hid_device(path: &str) -> bool {
    (path.starts_with("/dev/hiddev") || path.starts_with("/dev/usb/hiddev"))
        && is_character_device(path)
}

fn is_character_device(path: &str) -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::metadata(path).map(|m| m.file_type().is_char_device()).unwrap_or(false)
}

fn asdcontrol(args: &[&str]) -> Option<String> {
    let output = Command::new("sudo")
        .arg("asdcontrol")
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The HID nodes to hand `asdcontrol --detect`: `/dev/usb/hiddev*` first, then
/// `/dev/hiddev*`, each in the order the shell's globs expand them.
fn hid_devices() -> Vec<String> {
    let mut devices = Vec::new();
    for dir in ["/dev/usb", "/dev"] {
        let prefix = if dir == "/dev/usb" { "/dev/usb/hiddev" } else { "/dev/hiddev" };
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                is_character_device(&entry.path().to_string_lossy()).then_some(name)
            })
            .filter(|name| name.starts_with("hiddev"))
            .map(|name| format!("{prefix}{name}"))
            .collect();
        found.sort();
        devices.append(&mut found);
    }
    devices
}

fn detect_device() -> Option<String> {
    let devices = hid_devices();
    if devices.is_empty() {
        return None;
    }

    let mut args: Vec<&str> = vec!["--detect"];
    args.extend(devices.iter().map(String::as_str));
    let output = asdcontrol(&args)?;
    for line in output.lines() {
        let path = line.split(':').next().unwrap_or("").trim();
        if looks_like_hid_device(path) {
            return Some(path.to_string());
        }
    }
    None
}

fn find_device() -> Option<String> {
    // A cached path is only trusted while it still names a hiddev character
    // device; anything else is re-detected rather than handed to asdcontrol.
    if let Some(cache) = device_cache() {
        if let Ok(cached) = fs::read_to_string(&cache) {
            let cached = cached.trim().to_string();
            if looks_like_hid_device(&cached) {
                return Some(cached);
            }
        }
    }

    let device = detect_device()?;
    if let Some(cache) = device_cache() {
        let _ = fs::write(cache, format!("{device}\n"));
    }
    Some(device)
}

/// The panel's brightness as a percentage of its 60000-unit ceiling.
pub fn current(device: &str) -> Option<i64> {
    let output = asdcontrol(&[device])?;
    for line in output.lines() {
        if let Some(value) = line.trim().strip_prefix("BRIGHTNESS=") {
            let raw: f64 = value.trim().parse().ok()?;
            return Some((raw * 100.0 / BRIGHTNESS_CEILING) as i64);
        }
    }
    None
}

/// Run a step, retrying once with a freshly detected device when the cached one
/// stops working, the way the shell script does after removing its cache.
pub fn apply(step: &str) -> Result<i64, String> {
    let step = step.strip_suffix("%-").map(|n| format!("-{n}%")).unwrap_or_else(|| step.to_string());

    let mut device = find_device().ok_or("No Apple Display HID device found")?;
    if asdcontrol(&[&device, "--", &step]).is_none() {
        if let Some(cache) = device_cache() {
            let _ = fs::remove_file(cache);
        }
        device = find_device().ok_or("No Apple Display HID device found")?;
        asdcontrol(&[&device, "--", &step]).ok_or("asdcontrol failed")?;
    }

    current(&device).ok_or_else(|| "could not read Apple Display brightness".into())
}

/// The percentage for the no-argument query form.
pub fn query() -> Result<i64, String> {
    let device = find_device().ok_or("No Apple Display HID device found")?;
    if let Some(percent) = current(&device) {
        return Ok(percent);
    }
    // Retry once against a freshly detected device, as the script does.
    if let Some(cache) = device_cache() {
        let _ = fs::remove_file(cache);
    }
    let device = find_device().ok_or("No Apple Display HID device found")?;
    current(&device).ok_or_else(|| "could not read Apple Display brightness".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_comes_from_the_60000_ceiling() {
        let parse = |raw: f64| (raw * 100.0 / BRIGHTNESS_CEILING) as i64;
        assert_eq!(parse(0.0), 0);
        assert_eq!(parse(60000.0), 100);
        assert_eq!(parse(30000.0), 50);
        assert_eq!(parse(29700.0), 49);
    }

    #[test]
    fn down_steps_are_rewritten_for_asdcontrol() {
        let rewrite = |step: &str| {
            step.strip_suffix("%-")
                .map(|n| format!("-{n}%"))
                .unwrap_or_else(|| step.to_string())
        };
        assert_eq!(rewrite("5%-"), "-5%");
        assert_eq!(rewrite("+5%"), "+5%");
        assert_eq!(rewrite("50%"), "50%");
    }

    #[test]
    fn only_hid_character_devices_are_accepted() {
        assert!(!looks_like_hid_device("/dev/null"));
        assert!(!looks_like_hid_device("/tmp/not-a-device"));
        assert!(!looks_like_hid_device("/dev/hiddev0"));
    }
}
