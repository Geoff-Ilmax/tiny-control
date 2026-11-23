# Tiny 3 preset save/delete — research notes (throwaway branch)

This branch is research only. It is never merged; nothing under `src/` changes.
Goal: map how the OBSBOT Tiny 3 (`3564:ff02`) stores, writes and deletes its
on-camera presets, so `preset save` / `preset delete` can be enabled safely later.

## Known so far (from PROTOCOL.md)
- Presets' settings (40-byte actions record) are readable via `0x3DC4` with slot index.
- Power-on pose (38 bytes) is readable via `0x3884`; boot actions via `0x3E84`.
- Preset *poses* are not readable by any path tried (`0x0C`, `0x3B44`, `0x3A44`, `0x3B04`).
- Tiny 2 encoders in device.rs: ADD `0x3944`, UPDATE `0x3E04`, DELETE `0x3984`,
  TRIG `0x39C4`, BOOT_POS_SET `0x3844`, BOOT_ACTIONS_SET `0x3E44` — unaudited.
- Writes caused a stored side effect on a Tiny 2 (contrast/saturation/sharpness
  zeroed in Desk mode). Treat every write as dangerous.

## Plan
1. Baseline snapshots (read-only) — `baseline/`.
2. USB captures of OBSBOT Center doing one action each — `captures/`.
3. Decode captures — `tools/`.
4. Write up the memory map — this file.
5. Only with explicit go-ahead: replay one write with before/after snapshots.

## Capture log
| # | Action in OBSBOT Center | File | Notes |
|---|---|---|---|

## Findings from OBSBOT Center capture (2026-11-23, Center start-up)

Captured with `vm/capture.sh` (QEMU usb-host pcap), decoded with `tools/v3dump.py`.
All on V3 selector 2, receiver 0x04 (AI). Flag 0x21 = GET carrying a u32 argument.

| cmd | request | reply | meaning (tentative) |
|---|---|---|---|
| `0x3D04` | GET, no args | `00 01 02` | **list of occupied preset slots, 0-based** (tinyctl calls these 1-3) |
| `0x3CC4` | GET, arg = slot (u32) | ASCII `Preset1` … | **preset name per slot** |
| `0x6804` | GET | f32 1.66 | current zoom ratio (matches power-on zoom) |
| `0x3F84` | GET, arg u32 (5, 6) | f32 1.0 / 0.0 | unknown per-index float |
| `0x2084` | GET, arg 4 | `01` | unknown |
| `0x5484` | GET, args (0, k) | `00` | polled ~1/s with k = 8, 11, … |
| `0x3444` | **SET** `06 00 00 00 00` | – | Center writes this at start-up — check for side effects |

Center does not read preset positions at start-up; that must happen on recall/edit.
