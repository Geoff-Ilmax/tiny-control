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

## Capture 2 — Center "update Test4" (13:33, same setup.pcap)

- 13:33:30 Center re-read the store: `0x3D04` → `00` (only slot 0 occupied) and `0x3CC4` slot 0 →
  `Test4`. **So the add + rename from capture 1 persisted on the camera.**
- 13:33:57 save over Test4:
  - GIM_STATE → i16@8 = 344, i16@10 = −704; zoom read f32 1.21
  - **`0x3944` PRESET_ADD again, same slot 0** — Center never sends `0x3E04` (PRESET_UPDATE) to a Tiny 3:
    `00 00 00 00 | 80 e4 | 70 0d | 00 00 | 79 00 | 64 ff 64 ff | 00 01 00 01 | 00 00 00 00`
  - `0x3A84` slot 0 `Test4` re-sent after the add.

### Tiny 3 preset record (0x3944), two samples

| offset | type | sample 1 | sample 2 | meaning |
|---|---|---|---|---|
| 0  | u32 | 0 | 0 | slot, 0-based |
| 4  | i16 | 5670 | −7040 | pan × 100 (= GIM_STATE i16@10 × 10) |
| 6  | i16 | −5040 | 3440 | tilt × 100 (= GIM_STATE i16@8 × 10, raw sign) |
| 8  | i16 | 0 | 0 | roll (always 0) |
| 10 | u16 | 200 | 121 | zoom × 100 (sample 2 matches live zoom 1.21 exactly; sample 1 had live 1.96) |
| 12 | i16 ×2 | −75, −75 | −156, −156 | box x0, y0 |
| 16 | i16 ×2 | 175, 175 | 256, 256 | box x1, y1 |
| 20 | u32 | 0 | 0 | — |

The box is always centred on 50 with width ≈ 50000 / (zoom × 100)
(250 × 200 = 50000; 412 × 121 = 49852), so it looks derived from zoom, probably a digital-zoom ROI.
A writer could compute it, or copy Center's formula once a third sample confirms it.

Also seen (not preset-related): at 13:33:30 Center wrote file paths to XU selector 4
(`/app/private/mask0.jpg`, `mask1.jpg`, `loop2.mp4`), apparently its resource channel.

## Capture 3 — Center "As Initial State" on Test4 (13:37:39, same setup.pcap)

All receiver 0x04, in this order, within 350 ms:

1. **`0x3A44` GET preset position**, request `00 00 00 00 00` (u32 slot **+ one extra 0 byte**, 5 bytes).
   Reply (24 bytes, same layout as the 0x3944 record):
   `00 00 00 00 | 80 e4 | 70 0d | 00 00 | 79 00 | 08 00 08 00 | 5b 00 5b 00 | 64 00 00 00`
   = slot 0, pan −70.40, tilt 34.40, roll 0, zoom 1.21, box (8, 8, 91, 91), trailer u32 100.
   **This is the preset-position read we were missing.** `tinyctl probe boot` sends 0x3A44 with a
   4-byte request and gets no reply; the 5th byte is the likely reason. (Read-only, so it can be tested.)
   Note the box and trailer differ from what was written (camera recomputes/normalises them).
2. **`0x3EC4` SET power-on pose** (new command, Tiny 3), same 24-byte i16 layout:
   `00 00 00 00 | 80 e4 | 6f 0d | 00 00 | 79 00 | 00 00 00 00 | 53 00 53 00 | 00 00 00 00`
   = slot 0, pan −70.40, tilt 34.39, zoom 1.21, box (0, 0, 83, 83), trailer 0.
   tinyctl's Tiny-2 path uses `0x3844` with floats instead.
3. `0x3DC4` GET preset settings slot 0 → 40 bytes, all `ff` except byte 8 = `80` and bytes 16–19 = 0
   (Test4 has no stored settings), then **`0x3E44` BOOT_ACTIONS_SET with exactly that record** —
   same as tinyctl does on the Tiny 2.
4. `0x1D04` GET preset OPA, request **8 bytes** (u32 slot, u32 sub-index 0 / 1) → 39 zero bytes each.
   tinyctl's probe sends 4 bytes and gets no reply — same pattern as 0x3A44.
5. `0x1C84` SET initial-position OPA, 40 bytes: u32 sub-index (0, 1) + the 36 zero bytes read in step 4.

To verify from Linux after the VM releases the camera: `tinyctl probe boot` should now report the
power-on pose as pan −70.4, tilt ±34.4, zoom 1.21 (0x3884 replies in floats).

## Verified from Linux after capture 3 (VM stopped, tiny-guard/tiny-remote paused)

- `tinyctl -c 1 probe boot`: power-on pose (0x3884, floats) now pan −70.4, tilt 34.4, zoom 1.21, and
  0x3E84 power-on settings equal Test4's record. **Center's 0x3EC4 + 0x3E44 write took effect.**
- `tinyctl preset list`: slot 1 occupied, 2 and 3 empty (matches 0x3D04 → `00`).
- `tools/v3get.py` (read-only GETs, allowlisted commands only):

| request | reply |
|---|---|
| `0x3A44` + `00 00 00 00` (4 bytes, what tinyctl sends) | flags 0x29, malformed (len 46, no payload) |
| **`0x3A44` + `00 00 00 00 00` (5 bytes)** | **flags 0x29, 24 bytes: Test4's pose, identical to Center's read** |
| `0x3A44` + `01 …` / `02 …` (empty slots) | flags 0x89 (error) |
| `0x1D04` + 4 bytes | error 0x89 |
| **`0x1D04` + 8 bytes (slot, sub-index)** | 39 bytes (zeros) |
| **`0x1C44` + `00 00 00 00`** | 39 bytes (zeros); tinyctl sends no argument and gets no reply |
| `0x3CC4` slot 0 | `Test4` |

**Preset positions can be read on the Tiny 3**, with a 5-byte request. This removes the reason
`preset power-on N` was blocked on the Tiny 3 ("the camera doesn't report where its presets point").

## Capture 4 — Center "delete Test4" (13:44, delete.pcap)

- Center start-up: `0x3D04` → `00`, `0x3CC4` slot 0 → `Test4`, plus its usual start-up writes
  `0x3444 06 00 00 00 00` and `0xADC2` (receiver 0x02, flags 0x45, `08 01 12 02 20 01` /
  `08 01 12 04 08 01 20 01` — looks like protobuf).
- **Delete = one command: `0x3984` `00 00 00 00`** (u32 slot, 0-based). Nothing else — Center does not
  touch the power-on pose or settings when a preset is deleted.

Checked from Linux afterwards (tiny-guard paused), compared with a snapshot taken just before:

- `0x3D04`, `0x3A44` slot 0, `0x3CC4` slot 0 now all return error 0x89/0x09; `tinyctl preset list`
  shows every slot empty. **Delete took effect.**
- Power-on pose and settings unchanged (still Test4's: −70.4 / 34.4 / 1.21).
- `status --raw` identical. Brightness, contrast, saturation, sharpness unchanged (all 50).
  **No sign of the Tiny 2 side effect** (zeroed contrast/saturation/sharpness).
- One change: `white_balance_automatic` 1 → 0 (temperature 4000). No standard UVC WB write appears in
  either capture, so a Center vendor write did it — likely at start-up, since it also happened in the
  first session and tiny-guard silently restored it. Restarting tiny-guard set it back to 1 within
  seconds. Suspects: `0x3444` or `0xADC2`. Worth a dedicated check before tinyctl replays anything.

## Summary: Tiny 3 preset protocol (receiver 0x04, V3 selector 2)

| action | command | payload |
|---|---|---|
| list occupied slots | GET `0x3D04` | – → list of u8 slot ids |
| read name | GET `0x3CC4` | u32 slot → ASCII |
| **read position** | GET `0x3A44` | **u32 slot + u8 0 (5 bytes)** → 24-byte record |
| read settings | GET `0x3DC4` | u32 slot → 40-byte record |
| read OPA | GET `0x1D04` | u32 slot + u32 sub-index (0, 1) → 39 bytes |
| **save / overwrite** | SET `0x3944` | 24-byte record (below); `0x3E04` is not used |
| set name | SET `0x3A84` | u32 slot + ASCII (sent after every save) |
| **delete** | SET `0x3984` | u32 slot |
| power-on pose | SET `0x3EC4` | 24-byte record (box/trailer differ: see capture 3) |
| power-on settings | SET `0x3E44` | 40-byte settings record copied from the preset |
| power-on OPA | SET `0x1C84` | u32 sub-index + 36 bytes copied from 0x1D04 |

Record: `u32 slot | i16 pan×100 | i16 tilt×100 (raw gimbal sign) | i16 roll=0 | u16 zoom×100 |
i16 x0, y0, x1, y1 (box centred on 50, width ≈ 50000/(zoom×100)) | u32 0`.

Still open: the box formula (a third sample), what `0x3444`/`0xADC2` do (white balance?),
and slot limits: Center offers exactly 3 slots (0–2) for the Tiny 3; treated as the limit, not tested further. Writes to slot 1 confirmed in capture 5.

## White balance investigation (point 1), 2026-11-23

**Run A — Center start-up, untouched** (`wb-center-startup.pcap`; Center auto-starts at Windows login).

- Before: tiny-guard running → `white_balance_automatic=1`, 4000 K. Pausing tiny-guard/tiny-remote: still 1.
- After stopping the VM (guard still paused): **`white_balance_automatic=0`**, 4000 K, red 141, blue 162.
  Brightness, contrast, saturation, sharpness unchanged (50). So Center's start-up alone reproduces it.
- Every host→camera write in the run:
  - Windows: `SET_CONFIGURATION` ×2, `SET_INTERFACE` on if 3, and CDC `0x20`/`0x22` on if 4
    (the camera exposes a CDC serial interface). **No standard UVC control writes at all.**
  - Center (from 13:50:42, 16 s after the first packet): selector-2 GETs, file paths on selector 4
    (`/app/private/mask0.jpg`, `mask1.jpg`, `loop2.mp4`), and only three V3 SETs:
    `0xADC2` (receiver 0x02) `08 01 12 02 20 01`, `0x3444` (receiver 0x04) `06 00 00 00 00`,
    `0xADC2` `08 01 12 04 08 01 20 01`.

Two explanations remain:
1. One of Center's writes (`0xADC2`, `0x3444`, or the selector-4 file writes) turns auto WB off.
2. The camera re-initialises when it is re-enumerated (VM attach/detach) and comes back with a
   stored manual-WB setting; tiny-guard normally hides this by re-enabling auto WB.

**Unplug test — no VM, no Windows, no Center.** tiny-guard and tiny-remote paused, auto WB confirmed
on (1, 4000 K), Tiny 3 physically unplugged and replugged (USB device 6 → 8).

- After replug: **`white_balance_automatic=0`**, temperature 4000, red/blue balance 0.
  Brightness, contrast, saturation, sharpness 50.
- Restarting tiny-guard set auto WB back to 1 within seconds.

**Conclusion:** the Tiny 3 powers up with manual white balance (4000 K) and comes back that way on
every re-enumeration. The change seen after the Center sessions is explained by the VM
attach/detach alone; tiny-guard was silently correcting it each time. Center's start-up writes
(`0xADC2`, `0x3444`, selector-4 file paths) are **not needed to explain it** and are not implicated
in any preset side effect. (Not strictly ruled out that they also touch WB; that would need a
Windows-only run with Center's autostart disabled, which is no longer needed for the preset work.)
Open, outside this research: why the camera's power-on WB is manual (camera default, or a value
persisted earlier).

## Capture 5 — preset with "Additional settings" (point 2), settings.pcap, 13:58–14:00

Center auto-start now disabled; Center opened by hand. Camera had no presets.
The user saved the position first, then edited the additional settings: single tracking, medium
speed, contrast/saturation/sharpness/hue 46/47/48/49, manual WB 4400 K.

1. 13:59:18 `0x3944` ADD **slot 1** (chosen by hand in Center to test a non-zero slot — Center lets the user pick the slot),
   then `0x3A84` default name `Preset2`, then `0x3A84` `Set5`.
2. 13:59:31 opening the settings editor: `0x3DC4` slot 1 (all unset), `0x3A44` slot 1 (pose),
   `0x39C4` PRESET_TRIG slot 1 (recall, `idx` + four f32 1.0 — same as tinyctl).
   While editing, Center applies values live: `0x5444` (tracking), `0x2AC2` receiver 0x02
   (WB: `ff …`, u32 4000 → 4400 at bytes 4–7), `0x2182` receiver 0x02, and UVC image controls.
3. **14:00:30 save of the settings, in this order:**
   - **`0x3D84` AI_SET_PRESETS_ACTIONS**: u32 slot + the 40-byte record
     `0b 06 ff ff ff ff ff ff 80 ff ff ff ff 05 30 11 00 00 00 00 2e 2f 30 31 00 ff 00 00 00 00 00 18 10 ff…`
     (single, medium, manual WB 4400, image 46/47/48/49 — matches the PROTOCOL.md table)
   - `0x1D44` set preset OPA ×2 (44 bytes: u32 slot, then `00 01 00 00 …` / `01 00 00 00 …`)
   - **`0x3E04` AI_SET_PRESET_UPDATE_ONLY with the i16 pose record**:
     `01 00 00 00 | c6 e4 | a2 03 | 00 00 | 79 00 | 64 00 64 00 | 2c 01 2c 01 | 00 00 00 00`
     (same pose and zoom; box written as (100,100,300,300))
   - `0x3A84` name again, `0x2AC2` WB again.

**So `0x3E04` is used by Center on the Tiny 3**, after `0x3D84`, with the i16 pose layout. The Tiny 2
damage came from sending it with tinyctl's float layout. A future writer should send exactly
Center's sequence and layouts, never the Tiny 2 float payload.

### Verified from Linux (tiny-guard paused), compared with a snapshot just before

- `0x3D04` → `01`; `0x3CC4` slot 1 → `Set5`; `tinyctl preset list` shows slot 2 (1-based) only.
- `0x3DC4` slot 1 returns **exactly the record `0x3D84` wrote**.
- `0x3A44` slot 1 pose matches; the box reads back as (8, 8, 91, 91) whatever was written
  ((−156…256) by ADD, (100…300) by UPDATE_ONLY, (0…83) by the power-on write). **The camera
  recomputes the box from zoom** (centre 50, width ≈ 100/zoom: 83 ≈ 100/1.21), so its written value
  appears not to matter.
- `0x1D04` slot 1 sub 0 → first byte `01`, rest 0; sub 1 → all 0.
- Power-on pose and settings unchanged (editing a preset does not touch the power-on copy).
- Live image controls now 46/47/48/49 and WB manual: Center's live preview, i.e. the preset's own
  values, not corruption. Nothing went to 0. Tracking off (Center's live state); `status --raw` bytes
  0x18 and 0x2B changed 02 → 00 with it. tiny-guard restores its configured state on restart.

### After capture 5: tracking will not turn back on

- `tinyctl -c 1 image reset` put contrast/saturation/sharpness/hue back to 50 (OK).
- `tinyctl -c 1 track normal` is **not confirmed**: status flicks to "hand" for a moment, then stays
  at category 0 (off). Repeated twice, sampled every 0.5 s for 8 s: always `0x18 = 00`, `0x1c = 00`.
- Before this session (after the unplug test, with tiny-guard running) `track normal` worked and
  `status --raw` had `0x18 = 02`, `0x2b = 02`. So the reconnect alone does not cause it; something in
  the Center session does.
- Last gimbal-control write in the session was `0x0244 01` (manual control released), so that is not it.
  No selector-6 tracking TLVs were sent. Live writes Center made while the settings editor was open:
  `0x39C4` recall slot 1, `0x5444` `[0, 4, 0]` and `[0, 5, 2]` (u32 triples), `0x2182` receiver 0x02
  `c8 00 00 00 00 32 00 00` / `… 19 …`, `0x2AC2` WB. Prime suspect: `0x5444`.

**Resolved — not a Center side effect.** After the camera was turned to face a person (hands and
face in view), `status --raw` already read `0x18 = 02` (normal), `0x1c = 00`, `0x2b = 01` before any
new command: tiny-guard's retry of the saved "normal" intent had succeeded. `track normal` then
confirmed, stable for 8 s. The Tiny 3 only reports/accepts normal tracking while a subject is in
view; with an empty scene the status drops to category 0 and tinyctl cannot confirm the change.
Byte `0x2b` varied 02 / 00 / 01 across these reads (00 with nobody in view) — possibly subject
detection; not decoded. `0x5444` is therefore no longer a suspect for tracking.
(Possible tinyctl follow-up, outside this research: an unconfirmed tracking change on the Tiny 3
may just mean nobody is in frame.)

## Replay from Linux — save, recall, delete (2026-11-23, approved by the user)

Tool: `tools/replay.py` (dry run unless `--send`; refuses non-Tiny 3, occupied slots for save, empty
slots for recall/delete). Its frames were first checked offline against nine of Center's captured
preset frames (3 × ADD, 3 × name, 2 × DELETE, 1 × recall): **all match byte for byte** over the
CRC-covered length. (Center's frames carry leftover buffer bytes after the payload; they are outside
len2/CRC and ignored.) One fix came out of that: Center's box half-width is `25000 // zoom100`
(rounded down).

tiny-guard and tiny-remote paused throughout; full read-only snapshot before each step
(status --raw, probe boot, preset list, v4l2 controls, every slot's name/pose/settings).

1. **Save** to empty slot 0, live pose pan −10.50 / tilt 5.10 / zoom 1.21:
   `0x3944` `00 00 00 00 e6 fb fe 01 00 00 79 00 64 ff 64 ff 00 01 00 01 00 00 00 00`, then
   `0x3A84` `00 00 00 00 "Replay1"`.
   → slot list `[1, 0]` (the camera lists presets in save order), slot 0 name `Replay1`, pose
   read back exactly as sent, settings record unset (`ff…`, as after a plain Center save).
   Set5 in slot 1 unchanged. **Snapshot diff: status block, power-on pose/settings and every image
   control identical**; only the preset list changed.
2. **Recall** slot 0 (`0x39C4` slot + four f32 1.0) after the user turned the camera to
   pan −65.4 / tilt 8.3: within 1 s the gimbal read pan −10.5 / tilt 5.0 and held there. **Works.**
3. **Delete** slot 0 (`0x3984` `00 00 00 00`) → slot list `[1]`, Set5 intact; preset list, power-on
   records and image controls identical to the first snapshot.
   Only difference: tracking off (status `0x18` 02 → 00, `0x2b` 01 → 03) — the recall stops tracking
   so the camera holds the preset view (PROTOCOL.md: a recall only moves). tiny-guard restored
   normal tracking within 2 s of restarting.

**Result: on the Tiny 3, preset save (ADD + name), recall and delete work from Linux with Center's
exact byte layout, with no side effects on stored settings.** The writes tinyctl would need are the
ones in the protocol summary above; the Tiny-2 float payload must never be sent to a Tiny 3.
Not yet replayed from Linux: the settings write (`0x3D84` + `0x1D44` + `0x3E04`) and the power-on
write (`0x3EC4` + `0x3E44` + `0x1C84`).

# Tiny 2 cross-check (2026-11-23, tiny2.pcap)

Goal: check tinyctl's existing Tiny 2 preset assumptions against what OBSBOT Center actually sends.
Backup of all three Tiny 2 presets taken first (kept privately outside the repo). Test slot: Preset3
(0-based slot 2). Center auto-start disabled; opened by hand.

## Center start-up on the Tiny 2

- Preset list: **`0x3B44` GIMBAL_PRESET_LIST** → `00 01 02`; names: **`0x3B04`** u32 slot → `Preset1…3`.
  (On the Tiny 3 Center uses `0x3D04` / `0x3CC4` instead; `0x3B44`/`0x3B04` don't answer there.)
- `0x1AC8` (receiver 0x0D) `touch /app/private/resolution.conf` — Center runs a shell-like command on
  the camera at start-up.

## Action 1 — recall Preset3, move, update Preset3 (14:28)

1. `0x3A44` u32 slot 2 (**4-byte request on the Tiny 2**, 5 on the Tiny 3) →
   `02 00 00 00 | f32 −34.9 | f32 −0.9 | f32 0 | f32 1.0 | 00 00 00 00` (floats, matches the backup).
2. `0x39C4` recall: u32 slot + four f32 1.0 — **identical to tinyctl**.
3. `0x3DC4` slot 2 → Preset3's settings record (matches the backup).
4. Save: `0x6604` GIM_STATE (i16@8 = 52, i16@10 = 365), `0x6804` zoom → f32 1.0, then
   **`0x3E04` PRESET_UPDATE** `02 00 00 00 | 00 00 12 42 (36.5) | 67 66 a6 40 (5.2) | 0 | 00 00 80 3f (1.0) | 00 00 7a c4 (−1000.0)`.
   No name re-send, no settings write.

**Center uses `0x3E04` to update a Tiny 2 preset, with exactly tinyctl's layout**
(u32 slot, f32 pan, tilt, roll, zoom, −1000 sentinel). So tinyctl's command and byte layout for
update are right. The earlier Tiny 2 incident (PROTOCOL.md) therefore did not come from the layout itself.

Differences that remain between tinyctl and Center for a Tiny 2 save:

- **Where the pose comes from.** Center reads the live pose from the camera: pan = GIM_STATE i16@10 / 10,
  tilt = i16@8 / 10 (raw sign), zoom = `0x6804` f32. tinyctl's `current_pose()` uses the UVC
  pan/tilt/zoom controls — which tinyctl itself notes go stale after vendor moves — and maps zoom
  linearly onto 1.0–2.0. A stale or mis-scaled pose would be stored as-is.
- Tilt sign: Center stores GIM_STATE's raw sign (+5.2 here). Whether UVC `tilt_absolute` has the same
  sign on the Tiny 2 is not yet checked.

## Action 2 — rename Preset3 (14:32)

One write: **`0x3A84`** `02 00 00 00 "Test3"` (u32 slot + ASCII, no terminator) — the same command and
layout as on the Tiny 3. tinyctl has no rename today; this would be it on both models.

## Action 3 — edit Test3's additional settings (14:33–14:34)

Opening the editor: `0x3DC4` slot 2 (record still Preset3's original — so action 1's lone `0x3E04`
did **not** touch the settings), `0x3A44` slot 2 (pose 36.5 / 5.2 from action 1), `0x39C4` recall.
While editing, Center applied values live: selector-6 TLV `16 02 01` (tracking → group, the same TLV
tinyctl uses), `0x0CC4` TRACK_SPEED `00` / `02`, and `0x29C2` / `0x2942` reads (receiver 0x02).

Save, in this order:
1. **`0x3D84`** u32 slot 2 + 40-byte record
   `09 01 ff ff ff ff ff ff 80 ff ff ff ff ff ff ff 01 01 00 08 38 39 3a 3b 00 …`
   (group, motion speed, blur 8, contrast/sat/sharp/hue 56/57/58/59).
2. `0x6604` GIM_STATE read, then **`0x3E04`** u32 slot 2 + f32 pan **3.6**, tilt **30.3**, 0, zoom 1.0, −1000.

No `0x1D44` OPA writes on the Tiny 2 (the Tiny 3 sends two).

**Note: Center re-saves the live gimbal pose with the settings.** Group tracking was switched on live
in the editor, the camera moved to follow, and Center stored that new position (36.5 / 5.2 → 3.6 / 30.3).
A writer that only wants to change settings should send the preset's stored pose, not the live one.
