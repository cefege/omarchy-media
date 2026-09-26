# omarchy-media

Native replacements for the Omarchy shell commands that run while you are
holding a key down.

`omarchy-brightness-display` is a 122-line bash script. On one brightness key
press it runs `omarchy-hyprland-monitor-focused` (bash + `hyprctl` + `jq`),
`omarchy-hyprland-monitor-focused-apple` (bash + `hyprctl` + `jq`),
`omarchy-hw-display` (bash), `brightnessctl` + `awk` to read, `brightnessctl`
to write, `brightnessctl` + `awk` to read again, and `omarchy-osd` (bash + `jq`
+ `omarchy-shell`). That is roughly sixteen processes for one step of a
brightness slider, and Hyprland marks those bindings `repeating = true`, so it
happens several times a second while the key is down.

This is the same work in one binary.

## What it replaces

| Command | Status |
|---|---|
| `omarchy-brightness-display` | complete: internal panel, DDC/CI, Apple displays, DPMS on/off, OSD |

The same argv, the same stdout, the same exit codes. The Hyprland bindings,
the menus, and anything scripted against the command keep working: a change to
Omarchy swaps the file that answers to that name for three lines that `exec`
this binary.

## Measured

On the machine this was developed on (aarch64, Hyprland, one internal panel),
20 invocations of the query form:

| | Per invocation | Processes |
|---|---:|---:|
| `omarchy-brightness-display` (bash) | 39 ms | ~16 |
| `omarchy-media brightness display` | 12–16 ms | 0 |

The native figure sits on this box's process-spawn floor — `/bin/true` costs
16 ms here — so the remaining time is `fork`/`exec`, not the work. On a machine
where spawning costs about a millisecond, the same comparison is roughly
16 ms against 1.5 ms. The honest summary is the process count: sixteen becomes
one.

Output is identical: both print `30` for the panel at 126/420, and a `+5%` step
takes both from 30% to 35%.

## What is still a child process

Two, both deliberate:

- **`brightnessctl`, on writes only.** `/sys/class/backlight/*/brightness` is
  root-owned on most machines. brightnessctl gets around that through
  systemd-logind, which is a permission model rather than an arithmetic one, so
  the write falls back to one `brightnessctl` call after a direct write is
  refused. Replacing that with login1's D-Bus call by hand would save one fork
  and is not done yet.
- **`qs ipc`, for the OSD.** The on-screen display is a Quickshell window, so
  showing it is a message to another process. `omarchy-osd` and `omarchy-shell`
  wrap that call in bash and `timeout`; this invokes `qs` directly and enforces
  the timeout in-process, which removes two of the four.

## Fidelity

The percentage arithmetic is brightnessctl's, because the shell reads through
brightnessctl and the OSD shows the number it produced:

- reading: `roundf(current / max * 100)` in `f32`
- writing: `roundf(percent / 100 * max)` in `f32`

The DDC branch keeps the shell's integer arithmetic instead, which is a
different rule: `(current * 100 + max / 2) / max`, and
`(target * max + 50) / 100` before `setvcp`. Both are in `src/ddc.rs` with
their own tests.

Cache files are the same files, in the same format, with the same expiry
windows (`$XDG_RUNTIME_DIR/omarchy-brightness-display-ddc/*.bus`,
`omarchy-brightness-display-apple.device`), so the shell and this binary can
take turns during a transition without confusing each other.

The one deliberate difference: a write that fails is an error. The shell ignores
`brightnessctl`'s exit status and carries on to show an OSD for a brightness
that did not change; this exits 1 with the reason on stderr. That path is only
reached when brightnessctl also fails.

## Install

```sh
cargo build --release
install -Dm755 target/release/omarchy-media /usr/bin/omarchy-media
ln -s omarchy-media /usr/bin/omarchy-brightness-display
```

The binary dispatches on the name it was invoked as, so the symlink is the
whole installation. An AUR `PKGBUILD` is included.

## Tests

```sh
cargo test
```

24 tests, all against the rules the shell implements: the JSON read from
Hyprland, the Apple-display filter, the lit-display check, the backlight device
heuristic, the percentage roundings on both backends, the DDC bus/connector
pairing, the step rule near the bottom of the range, and the OSD payload.

## Licence

MIT.
