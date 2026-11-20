# Contributing

Thanks for helping! The most valuable contribution right now is **testing on
real hardware** — especially the Tiny 2 and Tiny 3, and multi-camera setups.

## Model owners: please test

The protocol is verified upstream on an **OBSBOT Tiny 3 Lite** (`3564:ff04`).
The Tiny 3 (`ff02`) and Tiny 2 (`fef8`) are verified for every command
(tracking speed is unsupported on the Tiny 3)
([`src/model.rs`](src/model.rs)). If you own one, please help:

1. **List your cameras** (reads sysfs only; opens nothing except a one-time,
   non-waking serial read):
   ```bash
   tinyctl list
   lsusb | grep -i 3564        # USB product IDs, if you have usbutils
   ```

2. **Try the low-impact commands first** (these don't move the gimbal).
   Replace `1` with the camera's number or alias:
   ```bash
   tinyctl -c 1 power     # truly read-only — reads sysfs, opens nothing
   tinyctl -c 1 status    # reads camera state; does NOT wake a sleeping camera
   tinyctl -c 1 info      # model, serial, UUID, USB port
   ```
   Note: `status`/`info` send a vendor request *frame* to the camera's
   command mailbox (a documented, non-destructive GET). Only `power`, `node`
   and `list` (once the serial is cached) touch nothing on the device.

3. **Then the fun ones**, watching the camera:
   ```bash
   tinyctl -c 1 sleep     # LED should go off, gimbal should park
   tinyctl -c 1 wake      # LED on, gimbal lifts
   tinyctl -c 1 track on ; tinyctl -c 1 track off
   for m in upper close headless lower group hand whiteboard desk; do
     tinyctl -c 1 track $m && echo "$m ok"; sleep 2
   done; tinyctl -c 1 track off
   tinyctl -c 1 pan 15 ; tinyctl -c 1 recenter
   tinyctl -c 1 wb temp 4000
   tinyctl -c 1 hdr on ; tinyctl -c 1 hdr off
   ```
   On-camera presets (the slots OBSBOT Center uses). Reading, recall and
   power-on are enabled; preset save/delete are disabled:
   ```bash
   tinyctl -c 1 preset list
   tinyctl -c 1 preset recall 1          # also applies the preset's settings
   tinyctl -c 1 preset power-on 1        # then replug: it should start there
   ```
   With several cameras, also check `tinyctl -c all status` and that changing
   one camera leaves the others alone.

4. **Open an issue** with: your exact model, the USB product ID, firmware
   revision (`bcdDevice` in `lsusb -v`), and which commands worked or printed
   "did not confirm". Frame captures of anything weird help a lot.

### Mapping a status byte

Readback offsets can differ per model (tracking speed moved from `0x21` to
`0x24` on the Tiny 2). `tinyctl -c 1 probe fov` (or
`face-ae`, `track-speed`) sets each value in turn, reads the 60-byte status
block after each, and prints the offsets whose byte follows the value. Please
report its output for your model; `tinyctl -c 1 status --raw` dumps the block.

### Recording a result / adding a model

- **Confirmed a feature?** Add it to that model's `verified` list in
  [`src/model.rs`](src/model.rs). **Found one missing?** Add it to
  `unsupported`, so `tinyctl` refuses it up front with a clear message.
- **New model?** Add a `Model` entry with its product ID to `MODELS` in
  `src/model.rs` (unknown OBSBOT PIDs already work as "unrecognized model"),
  and add a line to [`packaging/udev/71-tiny-control.rules`](packaging/udev/71-tiny-control.rules)
  mirroring the existing entries for group access + the cold-plug WB pin.

## Development

- Rust, one dependency (`libc`). `cargo build` / `cargo test`.
- `cargo test` runs the CRC, frame-codec, config, camera-selection and
  sysfs-discovery unit tests offline (no hardware; discovery uses a fake sysfs).
- Please keep the firmware-quirk comments intact and add to
  [`PROTOCOL.md`](PROTOCOL.md) when you discover something — the documentation
  is as much the point of this project as the tools.

## Safety when probing the protocol

- **Never blind-sweep the vendor command surface.** On the Tiny 2, a single pass
  over all GET opcodes took the device off the USB bus. Probe in small batches,
  liveness-check between commands, and save results incrementally.
- Treat the Upgrade subsystem (beyond serial/UUID reads), BLE, and any
  firmware-flash commands as off-limits without a recovery plan.
- Verify image-affecting changes with an actual captured frame, not just control
  readback — readbacks can look correct while the image is broken.
