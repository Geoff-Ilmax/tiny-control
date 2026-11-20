# tiny-control

Linux control suite for **OBSBOT Tiny 2 and Tiny 3 series** webcams — for one
camera or up to four at once. Sleep/wake, AI tracking, gimbal, and white
balance from the command line, a guard daemon that keeps each camera's settings,
and first-class [Omarchy](https://omarchy.org/)/Hyprland integration.

This is a fork of Josh Lambert's
[obsbot-tiny3-linux](https://github.com/joshualambert/obsbot-tiny3-linux),
which cracked the Tiny 3 control protocol. It adds:

- **Tiny 2 support** next to the Tiny 3 family (both speak the same UVC
  vendor protocol; see [`PROTOCOL.md`](PROTOCOL.md)).
- **Multiple cameras**, in the spirit of OBSBOT Center: every camera is
  identified by its serial number, has its own name, settings and presets, and
  is managed by a single guard daemon.

OBSBOT Center is Windows/macOS-only. [Tiny4Linux](https://github.com/OpenFoxes/Tiny4Linux)
is another good Tiny 2 tool.

## Features

| Feature | `tinyctl` | Notes |
|---|---|---|
| **Multi-camera** | `tinyctl list`, `-c <camera>`, `-c all` | Serial-based identity, aliases, per-camera settings |
| **Sleep / wake** | `sleep` / `wake` / `toggle` | Real vendor sleep: LED off, gimbal parks |
| **AI tracking** | `track on\|off\|<mode>` | normal, group, hand, whiteboard, desk; upper/lower body, close-up, headless on the Tiny 2 only |
| **Gimbal** | `pan/tilt/zoom`, `recenter` | Absolute PTZ (range queried per model) + the camera's own presets 1–3 and power-on position |
| **White balance** | `wb temp <K>` / `wb auto` | Manual color temperature, quirk-safe |
| **HDR / FOV / exposure** | `hdr`, `fov`, `exposure` | |
| **Status** | `status` / `info` | Reads state without waking the camera |
| **OBSBOT remote** | `tiny-remote` | The Tiny Smart Remote drives the selected camera; buttons 1–4 switch cameras |
| **Settings guard** | `tiny-guard` | One daemon for all cameras; protects audio-only calls |

## Supported models

All share one control protocol; only the USB product ID differs.

| Model | USB ID | Status |
|---|---|---|
| OBSBOT Tiny 3 Lite | `3564:ff04` | ✅ **Verified** upstream on real hardware (firmware 0510); framing modes treated as unsupported, like the Tiny 3 |
| OBSBOT Tiny 3 | `3564:ff02` | ✅ **Verified**: every command, except tracking speed and the upper/close/headless/lower framing modes (not supported on this model). Presets: list + recall verified; power-on via OBSBOT Center |
| OBSBOT Tiny 2 | `3564:fef8` | ✅ **Verified**: every command |
| Other OBSBOT (`3564:*`) | | ⚠️ Detected as "unrecognized model", every feature treated as untested |

Verified on a Tiny 2 and a Tiny 3 running together. `status` reads every
setting back from the camera, including FOV, face-AE and (on the Tiny 2)
tracking speed. For a new model, `tinyctl -c <camera> probe
fov|face-ae|track-speed` finds the status byte that reports each one.

**Untested means loud, not silent.** Each feature has a support level per
model ([`src/model.rs`](src/model.rs)): features known to be missing are
refused before anything is sent, and on untested models every setting is read
back — if the camera doesn't confirm it, `tinyctl` tells you which feature on
which model, e.g.:

```
tinyctl: 'whiteboard' tracking (sent, but the camera did not confirm it; untested on this model) is not supported on the OBSBOT Tiny 2
```

## Install

Every release is an **immutable tag** with checksummed, provenance-attested
assets — statically linked binaries for `x86_64` and `aarch64`, `.deb` / `.rpm` /
Arch packages, and a source tarball the PKGBUILD pins by `sha256`. See
[**Releases**](https://github.com/Geoff-Ilmax/tiny-control/releases).

> **Coming from obsbot-tiny3-linux?** The names changed (`t3ctl` → `tinyctl`,
> `t3-wb-guard` → `tiny-guard`, `/etc/obsbot-tiny3` → `/etc/tiny-control`).
> `install.sh` disables the old `t3-wb-guard.service` so the two guards don't
> fight; remove the old binaries and `/etc/udev/rules.d/71-obsbot-tiny3.rules`
> when you're ready. Old settings are not migrated.

### Arch / Omarchy (from a git checkout)

Requires a Rust toolchain (`rustup` or the `rust` package); `mpv` for the
optional preview.

```bash
git clone https://github.com/Geoff-Ilmax/tiny-control
cd tiny-control
./install.sh                       # builds + installs to ~/.local/bin, enables tiny-guard
sudo ./packaging/install-root.sh   # udev rule + system config (one-time, needs root)
```

Or build the package: `makepkg -si` with the `PKGBUILD` published with each
release (it pins the release's own source tarball by `sha256`).

### Prebuilt binaries (any distro)

```bash
VER=0.2.0
ARCH=x86_64          # or: aarch64
REL=https://github.com/Geoff-Ilmax/tiny-control/releases/download/v$VER

curl -fLO "$REL/tiny-control-$VER-$ARCH-linux-musl.tar.gz"
curl -fLO "$REL/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS      # must print: OK

tar xzf "tiny-control-$VER-$ARCH-linux-musl.tar.gz"
cd "tiny-control-$VER-$ARCH-linux-musl"
./install.sh
sudo ./packaging/install-root.sh
```

Every asset also carries a Sigstore build-provenance attestation:

```bash
gh attestation verify "tiny-control-$VER-$ARCH-linux-musl.tar.gz" --repo Geoff-Ilmax/tiny-control
```

Distro packages (`.deb`, `.rpm`, `.pkg.tar.zst`) are on the release page; then,
per user: `systemctl --user enable --now tiny-guard.service`.

`./install.sh` installs `tinyctl`, `tiny-guard` and `tiny-preview` to
`~/.local/bin` plus a per-user systemd unit; it needs no root. The system-wide
default config lives at `/etc/tiny-control/config`; per-user settings live in
`~/.config/tiny-control/config`.

## Usage

### Pick a camera

Every device command needs `-c` — there is no implicit default camera, even
when only one is plugged in.

```bash
tinyctl list
#   #  ID               ALIAS      MODEL                USB      NODE
# * 1  RMOWAAAAAAAAAA   -          Tiny 3               1-4.4    /dev/video2
#   2  RMOWBBBBBBBBBB   -          Tiny 2               3-4.1    /dev/video0

tinyctl alias 1 main           # name cameras once...
tinyctl alias 2 overhead
tinyctl -c main status         # ...then use the name

tinyctl select overhead        # remember a "selected" camera (bar widget + hotkeys)
tinyctl -c selected toggle
tinyctl select next            # cycle the selection
tinyctl -c all sleep           # every camera, one after another
```

`-c` accepts an alias, the serial (`ID`), the list number, the USB port
(`1-4.4`), a device path (`/dev/video2` or a `/dev/v4l/by-*` link),
`selected`, or `all`. Aliases and settings follow the camera's serial, so they
survive moving it to another port. (If a camera's serial can't be read, it is
keyed by USB port instead, shown as `port-1-4.4`.)

### Control it

These examples use `-c 1` (the first camera in `tinyctl list`); any of the
selectors above works in its place — an alias such as `-c main`, a serial,
`-c selected` or `-c all`.

```bash
tinyctl -c 1 sleep                # camera off (LED off, gimbal parks)
tinyctl -c 1 wake                 # back on
tinyctl -c 1 toggle               # sleep <-> wake (prints the new state)
tinyctl -c 1 status               # current state (does NOT wake a sleeping camera)
tinyctl -c 1 info                 # model, serial number, UUID, USB port

tinyctl -c 1 track on             # AI subject tracking
tinyctl -c 1 track desk           # desk mode (any mode in `tinyctl --help`)
tinyctl -c 1 track upper          # upper-body framing (Tiny 2 only)
tinyctl -c 1 track off

tinyctl -c 1 pan 20               # absolute pan, degrees (range shown by `status`)
tinyctl -c 1 tilt -10
tinyctl -c 1 zoom 60              # 0..100
tinyctl -c 1 recenter
tinyctl -c 1 preset list          # the camera's own slots 1-3 + power-on position
tinyctl -c 1 preset recall 2      # same slots OBSBOT Center uses; also applies the
                                  # preset's Additional settings (tracking, image, WB)
tinyctl -c 1 preset power-on 2    # power on at preset 2 with its Additional settings
# preset save / delete and power-on off are disabled: an earlier save wrote a
# Tiny 2's preset settings record wrongly. Use OBSBOT Center for those.

tinyctl -c 1 wb temp 4000         # manual white balance, 4000K
tinyctl -c 1 wb auto
tinyctl -c 1 hdr on
tinyctl -c 1 exposure auto        # or: exposure 330
tinyctl -c 1 image                # hue, contrast, saturation, sharpness
tinyctl -c 1 image hue 50         # set one (1..100); `image reset` for defaults
tinyctl -c 1 support              # what is verified / unsupported on this model

tinyctl -c 1 --json status        # machine-readable output
tinyctl list --json               # camera list for scripts / the bar widget
```

With `-c all --json`, one JSON object is printed per camera, one per line; every
object carries `camera`, `alias`, `model` and `model_id`. Run `tinyctl --help`
for the full command list.

### Persistent settings and call protection

`tinyctl` saves each camera's choices in its own `[camera <serial>]` section of
`~/.config/tiny-control/config` (see [`packaging/config.example`](packaging/config.example));
keys outside a section are defaults for every camera. `tiny-guard` reconciles
**every connected camera** every second — power, tracking, HDR and white
balance — and reloads changes without a restart. Hot-plugged cameras are picked
up automatically. CLI and guard operations share a lock so they cannot
overwrite one another mid-command. Commands check camera readback and report
failures; a saved setting remains pending if firmware rejects it, and the guard
retries.

- `wake`: disable firmware auto-sleep and stay awake until Sleep/Auto.
- `sleep`: explicit sleep overrides active calls and firmware wake attempts.
- `auto`: wake when an app uses that camera's video **or its microphone**;
  release the keep-awake descriptor when idle. This is the default.
- `track off`: tracking stays off during use even if firmware changes it.
- `wb auto`: auto WB stays selected; the guard does not fight it.

The guard matches each camera's ALSA capture to the same physical USB device,
so one camera's microphone keeps only that camera awake. It reads capture
activity from procfs; it does not record or start video capture.

The firmware auto-sleep timer is separate from USB autosuspend: microphone
capture and an open control descriptor do not prevent its countdown. Whenever
Awake or an active Auto call requires a camera awake, the guard sets the timer
to zero and verifies readback. On models where this timer is untested, a
failure is reported once as a warning and the camera is still woken.
`tinyctl -c <camera> status --json` reports `auto_sleep_seconds`.

Defaults are tracking off and manual WB at 4000K. HDR remains unmanaged until
explicitly selected. Reconciliation is corrective, not an atomic firmware lock:
external voice, gesture or app changes can take effect briefly before the next
guard tick.

### Optional call microphone (PipeWire)

`packaging/install-audio.sh` installs a separate user service exposing
**OBSBOT Calls (Speech Optimized)** (source name `obsbot_calls`): WebRTC echo
cancellation → RNNoise → voice EQ → compressor → limiter. It currently targets
a single camera's microphone, by its PipeWire source name in
`~/.config/tiny-control/tiny-audio.conf` — edit `target.object` for your model
(`pactl list short sources` shows the names). Per-camera processed microphones
are planned.

On Arch/Omarchy, install `noise-suppression-for-voice` and `swh-plugins` first
(`omarchy pkg add noise-suppression-for-voice swh-plugins`).

```bash
./packaging/install-audio.sh
pactl set-default-source obsbot_calls
```

To undo: select the original microphone and run
`systemctl --user disable --now tiny-audio.service`. Tuning notes are in the
comments of [`packaging/pipewire/tiny-audio.conf`](packaging/pipewire/tiny-audio.conf).

## OBSBOT Tiny Smart Remote

The remote's USB dongle (`1106:b106`) works without OBSBOT Center. It is a
plain keyboard that sends a Ctrl+Alt shortcut per button; `tiny-remote` (a
per-user service enabled by `install.sh`) reads it and runs the matching
`tinyctl` command on the **selected** camera — the same one the bar widget
and keybindings use. It grabs the dongle, so those shortcuts don't also reach
other apps (Ctrl+Alt+T would otherwise open a terminal on many desktops).

| Button | Action |
|---|---|
| Power | sleep / wake |
| Track | AI tracking on / off |
| Close-up, Hand, Whiteboard, Desk | switch to that mode; press again for tracking off |
| ▲ ▼ ◀ ▶ (hold to repeat) | pan / tilt — 5° steps, or 1° with the camera's Fine control; honours its Swap left/right and Swap up/down |
| Center | recenter |
| Zoom − / + | zoom out / in |
| P1 P2 P3 | recall that on-camera preset (with its settings) |
| 1 2 3 4 | select camera 1–4 (order of `tinyctl list`), with a notification |

The udev rule (`sudo ./packaging/install-root.sh`, then replug the dongle)
gives your login session access to it. Check it with
`systemctl --user status tiny-remote`; `tiny-remote --dry-run --no-grab` in a
terminal prints the command each button would run instead of running it.
Tiny 3 limits still apply (no Close-up there; the remote reports it).

## Omarchy / Hyprland integration

- **Quickshell bar widget** — a separate, optional add-on with a camera
  drop-down: **[Geoff-Ilmax/omarchy-tiny-control](https://github.com/Geoff-Ilmax/omarchy-tiny-control)**.
  The camera chosen there becomes `selected`, which the hotkeys and menu act on.
- **Live preview** — `tiny-preview <camera> [WxH]` opens a small, pinned,
  corner self-view (mpv) of that camera. It streams only while open; calling it
  again closes it, and calling it for another camera switches over. The
  Hyprland float rule is in [`packaging/hypr/windows.omarchy.lua`](packaging/hypr/windows.omarchy.lua).
- **Keybindings** — copy from [`packaging/hypr/bindings.omarchy.lua`](packaging/hypr/bindings.omarchy.lua)
  into `~/.config/hypr/bindings.lua` (or the vanilla `.conf` form). They act on
  the **selected** camera:
  - `SUPER + ALT + C` — sleep toggle (with a notification)
  - `SUPER + ALT + R` — recenter gimbal
  - `SUPER + ALT + T` — tracking toggle
  - `SUPER + ALT + N` — select the next camera

  To pin a key to one camera regardless of the selection, use its alias:
  `tinyctl -c 1 toggle`.
- **Menu** — merge [`packaging/omarchy-menu.jsonc`](packaging/omarchy-menu.jsonc)
  into `~/.config/omarchy/extensions/omarchy-menu.jsonc` for a **Camera** submenu.
- **systemd** — [`packaging/systemd/tiny-guard.service`](packaging/systemd/tiny-guard.service)
  is a hardened per-user unit.

## Firmware quirks (the hard-won ones)

Documented in full in [`PROTOCOL.md`](PROTOCOL.md) and encoded in the code:

1. **Last-written-control-wins white balance.** `white_balance_temperature`
   must be written *last*. Writing red/blue balance afterwards turns them into
   literal channel gains — a saturated **neon-green** image, while readbacks
   still look fine. `tinyctl` and the guard always use the safe order.
2. **Starting a capture wakes the camera; control reads don't.** `status`,
   `info` and the serial read used for identification work on a sleeping camera
   without waking it. `list`, `node` and `power` read only sysfs.
3. **Pan/tilt readback is cached by uvcvideo**, so `recenter` uses the UVC
   pan/tilt controls (not the vendor frame) to keep readback honest. After AI
   tracking moves the gimbal, `status` and `preset save` still show the last
   host-commanded angle; save presets from a position set with `pan/tilt`.
5. **Presets live in the camera.** `preset` uses the camera's three slots, so
   presets made in OBSBOT Center work here and the other way round. The
   power-on position is a separate pose stored in the camera; `preset power-on
   N` copies slot N's pose into it (changing slot N later does not move it).
4. **No USB serial string.** These cameras publish no iSerial, so two identical
   models get the same `/dev/v4l/by-id` name. Cameras are found via sysfs and
   identified by the vendor serial instead.

## How it works

The cameras expose a UVC extension unit (bUnitID 2, GUID
`9A1E7291-6843-4683-6D92-39BC7906EE49`). Vendor commands are 60-byte framed
"V3" messages (magic `0xAA`, CRC-16/USB checksums) on selector 2; simpler
settings (AI tracking, HDR, FOV) are raw TLVs on selector 6; standard image and
PTZ controls are plain V4L2/UVC. No proprietary SDK is required. See
[`PROTOCOL.md`](PROTOCOL.md) for every command, byte, and dead end.

## Acknowledgments

- **[joshualambert/obsbot-tiny3-linux](https://github.com/joshualambert/obsbot-tiny3-linux)**
  (MIT) — the project this is forked from, and the Tiny 3 protocol work.
- **[OpenFoxes/Tiny4Linux](https://github.com/OpenFoxes/Tiny4Linux)** (EUPL-1.2)
  — the Tiny 2 control tool; its command frames were the Rosetta stone for the
  V3 protocol, and its notes informed the Tiny 2 status decoding here.
- **[brendanwelsh/obsbot-tiny3-protocol](https://github.com/brendanwelsh/obsbot-tiny3-protocol)**
  (MIT) — measured Tiny 3 command behaviour; see the cross-reference in
  [`PROTOCOL.md`](PROTOCOL.md).
- **[lxman/obsbot-mcp](https://github.com/lxman/obsbot-mcp)** (MIT),
  **[samliddicott/meet4k](https://github.com/samliddicott/meet4k)**,
  **[cgevans/tiny2](https://github.com/cgevans/tiny2)**, and
  **[taxfromdk/obsbot_tiny_reversing](https://github.com/taxfromdk/obsbot_tiny_reversing)**
  — the lineage of OBSBOT XU reverse-engineering.

Protocol *facts* (opcodes, CRC parameters, frame layout) are not copyrightable;
all code here is original and MIT-licensed.

## Contributing

Owners of any OBSBOT model: **please test and report** — see
[`CONTRIBUTING.md`](CONTRIBUTING.md). A `tinyctl list`, `tinyctl -c 1 info` and
`tinyctl -c 1 status` dump plus your USB product ID helps confirm the model
matrix.

## License

MIT — see [`LICENSE`](LICENSE).
