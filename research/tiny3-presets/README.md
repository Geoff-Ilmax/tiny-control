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

## Capture 1 — Center "add preset" (13:31, same setup.pcap)

Sequence Center sent (receiver 0x04 unless noted):

1. 13:31:14–18 `0x3984` PRESET_DELETE `00 00 00 00`, `01 …`, `02 …` — **all three existing presets
   deleted** (2 s apart; confirmed: the user deleted them by hand in Center, not Center itself). Delete = u32 slot, 0-based — same as tinyctl.
2. 13:31:23–26 manual move: `0x0244 00` (enter manual?) … `0x6484` gimbal speed stream … `0x0244 01`.
   Zoom via `0x1942` (receiver 0x02) `08 00 00 00 <zoom×100 u32>` (0xa6 → 0xc4 = 1.66 → 1.96).
3. 13:31:36 save:
   - `0x6604` GIM_STATE read → `3f 00 41 fe 29 f9 00 00 08 fe 37 02 …` (i16@8 = −504, i16@10 = 567)
   - `0x6804` zoom read → f32 1.96
   - **`0x3944` PRESET_ADD, 24-byte payload:**
     ```
     00 00 00 00   slot u32 (0-based)
     26 16         i16  5670 = pan  56.70°  (GIM_STATE i16@10 × 10)
     50 ec         i16 −5040 = tilt −50.40° (GIM_STATE i16@8 × 10, raw sign — tinyctl displays it negated)
     00 00         i16 0 (roll?)
     c8 00         u16 200 (zoom? live zoom read 1.96 — check against a second sample)
     b5 ff b5 ff   i16 −75, −75   unknown
     af 00 af 00   i16 175, 175   unknown
     00 00 00 00   u32 0
     ```
   - `0x3A84` **set preset name**: u32 slot + ASCII, sent right after ADD with default `Preset1`.
4. 13:31:55 `0x3A84` `00 00 00 00 "Test4"` — the rename.

**Key result:** the Tiny 3 ADD record is fixed-point i16 (degrees × 100), not tinyctl's
Tiny-2 layout (u32 slot + five f32 with a −1000 sentinel). Sending tinyctl's payload to a Tiny 3
would be misread, which fits "the Tiny 3 throws away preset writes".
