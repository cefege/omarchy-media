//! The on-screen display.
//!
//! `omarchy-osd` builds a JSON payload with `jq` and hands it to
//! `omarchy-shell`, which is a bash script that runs `qs ipc` under `timeout`.
//! Four processes to show a number on screen. This builds the same payload and
//! runs the same `qs` call, with the timeout enforced in-process, which is the
//! one process left.
//!
//! The payload's shape matters: it is parsed by the shell side, and `jq -cn`
//! with `--arg` makes every value a string, empty strings included.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A Go duration, which is what `OMARCHY_SHELL_IPC_TIMEOUT` holds.
fn parse_timeout(text: &str) -> Duration {
    let text = text.trim();
    let (digits, multiplier) = if let Some(value) = text.strip_suffix("ms") {
        (value, 0.001)
    } else if let Some(value) = text.strip_suffix('s') {
        (value, 1.0)
    } else if let Some(value) = text.strip_suffix('m') {
        (value, 60.0)
    } else {
        (text, 1.0)
    };

    digits
        .parse::<f64>()
        .map(|value| Duration::from_secs_f64(value * multiplier))
        .unwrap_or(Duration::from_secs(2))
}

fn ipc_timeout() -> Duration {
    match std::env::var("OMARCHY_SHELL_IPC_TIMEOUT") {
        Ok(value) => parse_timeout(&value),
        Err(_) => Duration::from_secs(2),
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The payload `omarchy-osd -i brightness -p N` produces.
pub fn brightness_payload(percent: i64) -> String {
    let percent = percent.to_string();
    format!(
        "{{\"icon\":\"brightness\",\"message\":\"\",\"value\":{0},\"progressText\":{1},\"max\":\"100\",\"duration\":\"\"}}",
        json_string(&percent),
        json_string(&format!("{percent}%"))
    )
}

/// `qs` matches instances by display, and a caller from outside the session
/// (ssh, or a service started before the compositor) has none.
fn recover_wayland_display() {
    if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()) {
        return;
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));

    let Ok(entries) = std::fs::read_dir(&runtime) else {
        return;
    };
    let mut candidates: Vec<(std::time::SystemTime, String)> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".lock") || !name.starts_with("wayland-") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, name))
        })
        .collect();
    candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));

    if let Some((_, name)) = candidates.first() {
        std::env::set_var("WAYLAND_DISPLAY", name);
    }
}

/// Send an OSD, best effort. Returns `Err` with a reason only when the caller
/// asked for the failure to be reported; the key-repeat path does not.
pub fn show(payload: &str, quiet: bool) -> Result<(), String> {
    let omarchy_path = match std::env::var_os("OMARCHY_PATH") {
        Some(path) if !path.is_empty() => path,
        _ => return fail(quiet, "OMARCHY_PATH is not set".to_string()),
    };
    let shell = PathBuf::from(&omarchy_path).join("shell");
    if !shell.join("shell.qml").is_file() {
        return fail(quiet, format!("omarchy-shell config not found: {}", shell.display()));
    }

    recover_wayland_display();

    let mut child = match Command::new("qs")
        .args(["ipc", "-n", "-p"])
        .arg(&shell)
        .args(["call", "--", "osd", "show", payload])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return fail(quiet, format!("could not run qs: {e}")),
    };

    let mut stdout = child.stdout.take();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(stdout) = stdout.as_mut() {
            let _ = stdout.read_to_end(&mut buffer);
        }
        let _ = sender.send(String::from_utf8_lossy(&buffer).into_owned());
    });

    let deadline = Instant::now() + ipc_timeout();
    let output = match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(output) => {
            let _ = child.wait();
            output
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return fail(quiet, "omarchy-shell is not responding".to_string());
        }
    };
    let _ = reader.join();

    if output.contains("Target not found.")
        || output.contains("Function not found.")
        || output.contains("Too few arguments provided")
        || output.contains("Too many arguments provided")
        || output.contains("Not ready to accept queries yet")
    {
        return fail(quiet, output.trim().to_string());
    }

    Ok(())
}

fn fail(quiet: bool, reason: String) -> Result<(), String> {
    if quiet {
        Ok(())
    } else {
        Err(reason)
    }
}

/// Show a brightness OSD, the way `omarchy-osd -i brightness -p N` does.
pub fn brightness(percent: i64, quiet: bool) -> Result<(), String> {
    show(&brightness_payload(percent), quiet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_matches_the_jq_shape() {
        assert_eq!(
            brightness_payload(30),
            r#"{"icon":"brightness","message":"","value":"30","progressText":"30%","max":"100","duration":""}"#
        );
        assert_eq!(
            brightness_payload(0),
            r#"{"icon":"brightness","message":"","value":"0","progressText":"0%","max":"100","duration":""}"#
        );
    }

    #[test]
    fn strings_are_escaped_for_the_shell_side() {
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_string("a\nb"), "\"a\\nb\"");
        assert_eq!(json_string("a\u{1}b"), "\"a\\u0001b\"");
        assert_eq!(json_string("plain"), "\"plain\"");
    }

    #[test]
    fn go_durations_parse() {
        assert_eq!(parse_timeout("2s"), Duration::from_secs(2));
        assert_eq!(parse_timeout("500ms"), Duration::from_millis(500));
        assert_eq!(parse_timeout("1m"), Duration::from_secs(60));
        assert_eq!(parse_timeout("nonsense"), Duration::from_secs(2));
    }
}
