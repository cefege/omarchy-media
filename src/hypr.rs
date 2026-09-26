//! Talking to Hyprland.
//!
//! Every volume and brightness keypress needs to know which monitor is
//! focused, and the shell scripts answered that by running `hyprctl monitors
//! -j` and piping it through `jq`. `hyprctl` is a client for a unix socket, so
//! this speaks to that socket directly: one connection, no child processes.
//! `hyprctl` is still used for the cold paths (DPMS dispatch) where its
//! argument handling is worth more than a saved fork.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;

use crate::json::{self, Json};

/// The monitors Hyprland knows about, as returned by its `monitors` request.
pub fn monitors() -> Result<Vec<Json>, String> {
    match socket_request("j/monitors") {
        Some(reply) => match json::parse(&reply)? {
            Json::Arr(monitors) => Ok(monitors),
            _ => Err("hyprland returned a monitor list that is not an array".into()),
        },
        // No socket, or Hyprland not running under this environment: the same
        // answer is still available through the client.
        None => hyprctl_monitors(),
    }
}

fn socket_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    Some(
        PathBuf::from(runtime)
            .join("hypr")
            .join(signature)
            .join(".socket.sock"),
    )
}

fn socket_request(command: &str) -> Option<String> {
    let mut stream = UnixStream::connect(socket_path()?).ok()?;
    stream.write_all(command.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut reply = String::new();
    stream.read_to_string(&mut reply).ok()?;
    Some(reply)
}

/// Whether the compositor's request socket is there, which decides whether a
/// press costs a socket round trip or a `hyprctl` process.
pub fn socket_reachable() -> bool {
    socket_path().is_some_and(|path| path.exists())
}

fn hyprctl_monitors() -> Result<Vec<Json>, String> {
    let output = Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
        .map_err(|e| format!("could not run hyprctl: {e}"))?;
    if !output.status.success() {
        return Err("hyprctl monitors failed".into());
    }
    match json::parse(&String::from_utf8_lossy(&output.stdout))? {
        Json::Arr(monitors) => Ok(monitors),
        _ => Err("hyprctl returned a monitor list that is not an array".into()),
    }
}

fn monitor_named<'a>(monitors: &'a [Json], name: Option<&str>) -> Option<&'a Json> {
    monitors.iter().find(|monitor| match name {
        Some(name) => monitor.get("name").and_then(Json::as_str) == Some(name),
        None => monitor.get("focused").map(Json::is_true).unwrap_or(false),
    })
}

/// The focused monitor's name, as `omarchy-hyprland-monitor-focused` prints
/// it. It takes an already-fetched list, so one key press pays for a single
/// socket round trip instead of one per question.
pub fn focused_in(monitors: &[Json]) -> Option<String> {
    monitor_named(monitors, None)?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

/// The filter `omarchy-hyprland-monitor-focused-apple` runs through jq: the
/// named monitor (or the focused one) has to be an Apple Studio or XDR display.
pub fn apple_for(monitors: &[Json], name: Option<&str>) -> bool {
    let Some(monitor) = monitor_named(monitors, name) else {
        return false;
    };

    monitor.get("make").and_then(Json::as_str) == Some("Apple Computer Inc")
        && monitor
            .get("model")
            .and_then(Json::as_str)
            .is_some_and(|model| {
                model.contains("StudioDisplay")
                    || model.contains("ProDisplayXDR")
                    || model.contains("Studio XDR")
            })
}

/// True when every display that is not disabled is already lit, so a DPMS
/// enable would only force a redundant modeset.
pub fn every_display_lit() -> bool {
    match monitors() {
        Ok(monitors) => all_lit(&monitors),
        Err(_) => false,
    }
}

pub fn all_lit(monitors: &[Json]) -> bool {
    let active: Vec<&Json> = monitors
        .iter()
        .filter(|monitor| monitor.get("disabled").and_then(Json::as_bool) == Some(false))
        .collect();

    !active.is_empty()
        && active
            .iter()
            .all(|monitor| monitor.get("dpmsStatus").and_then(Json::as_bool) == Some(true))
}

/// Turn a monitor on or off through `hyprctl dispatch`.
pub fn dispatch_dpms(enable: bool) -> Result<(), String> {
    let action = if enable { "enable" } else { "disable" };
    let status = Command::new("hyprctl")
        .args(["dispatch", &format!("hl.dsp.dpms({{ action = \"{action}\" }})")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("could not run hyprctl: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err("hyprctl dispatch failed".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str = r#"[
        {"id":0,"name":"eDP-1","make":"LENOVO","model":"21N9","focused":true,"disabled":false,"dpmsStatus":true},
        {"id":1,"name":"DP-2","make":"Apple Computer Inc","model":"StudioDisplay","focused":false,"disabled":false,"dpmsStatus":true}
    ]"#;

    fn monitors_of(json_text: &str) -> Vec<Json> {
        json::parse(json_text).unwrap().as_array().unwrap().to_vec()
    }

    #[test]
    fn focused_monitor_comes_from_the_focused_flag() {
        let monitors = monitors_of(MONITORS);
        assert_eq!(focused_in(&monitors).as_deref(), Some("eDP-1"));
        assert_eq!(focused_in(&monitors_of("[]")), None);
        // With nothing focused, the shell's jq prints nothing and the caller
        // carries on with an empty monitor name.
        assert_eq!(focused_in(&monitors_of(r#"[{"name":"eDP-1","focused":false}]"#)), None);
    }

    #[test]
    fn apple_detection_matches_the_jq_filter() {
        let monitors = monitors_of(MONITORS);

        // The focused monitor is the internal panel, not the Apple one.
        assert!(!apple_for(&monitors, None));
        assert!(!apple_for(&monitors, Some("eDP-1")));
        assert!(apple_for(&monitors, Some("DP-2")));
        assert!(!apple_for(&monitors, Some("DP-3")));
    }

    #[test]
    fn apple_detection_needs_both_make_and_model() {
        let wrong_make = monitors_of(
            r#"[{"name":"DP-2","make":"Dell Inc.","model":"StudioDisplay","focused":true}]"#,
        );
        assert!(!apple_for(&wrong_make, Some("DP-2")));

        let wrong_model = monitors_of(
            r#"[{"name":"DP-2","make":"Apple Computer Inc","model":"MacBook Pro","focused":true}]"#,
        );
        assert!(!apple_for(&wrong_model, Some("DP-2")));

        for model in ["StudioDisplay", "ProDisplayXDR", "Studio XDR"] {
            let hit = monitors_of(&format!(
                r#"[{{"name":"DP-2","make":"Apple Computer Inc","model":"{model}","focused":true}}]"#
            ));
            assert!(apple_for(&hit, Some("DP-2")), "{model} should match");
        }
    }

    #[test]
    fn every_display_lit_ignores_disabled_monitors() {
        assert!(all_lit(&monitors_of(MONITORS)));

        // A dark active display means a DPMS enable is still needed, even when
        // the other one is already lit.
        let one_dark = monitors_of(
            r#"[{"name":"eDP-1","disabled":false,"dpmsStatus":true},
                {"name":"DP-2","disabled":false,"dpmsStatus":false}]"#,
        );
        assert!(!all_lit(&one_dark));

        // Disabled monitors do not count either way: a dark one that is off
        // does not make the panel flash on resume.
        let one_off = monitors_of(
            r#"[{"name":"eDP-1","disabled":false,"dpmsStatus":true},
                {"name":"DP-2","disabled":true,"dpmsStatus":false}]"#,
        );
        assert!(all_lit(&one_off));

        // Nothing active at all is not "everything is lit".
        assert!(!all_lit(&monitors_of(
            r#"[{"name":"DP-2","disabled":true,"dpmsStatus":false}]"#
        )));
    }
}

