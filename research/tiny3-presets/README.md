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
