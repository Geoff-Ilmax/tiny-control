#!/usr/bin/env python3
"""Throwaway research tool: replay OBSBOT Center's Tiny 3 preset writes from Linux.

Mirrors, byte for byte, what Center sent in the captures (see ../README.md):
  save   = 0x3944 PRESET_ADD (24-byte i16 record) then 0x3A84 set name
  recall = 0x39C4 (u32 slot + four f32 1.0)
  delete = 0x3984 (u32 slot)
All receiver 0x04, V3 frames on XU unit 2 selector 2 (format: src/frame.rs).

Nothing is written unless --send is given. Without it, every command only
reads (gimbal state, zoom, slot list) and prints the exact frames it would send.

usage:
  replay.py /dev/videoN save   SLOT NAME [--send]   SLOT is 0-based (0..2); must be empty
  replay.py /dev/videoN recall SLOT      [--send]   SLOT must be occupied
  replay.py /dev/videoN delete SLOT      [--send]   SLOT must be occupied
  replay.py /dev/videoN read                        slot list, names, poses, settings
"""
import ctypes, fcntl, os, struct, sys, time

TINY3 = ("3564", "ff02")
SLOTS = 3
RCV_AI, XU_UNIT, SEL_CMD = 0x04, 2, 2
UVC_SET_CUR, UVC_GET_CUR = 0x01, 0x81
UVCIOC_CTRL_QUERY = 0xC0107521
FLAG_GET, FLAG_GET_ARGS, FLAG_SET = 0x01, 0x21, 0x25
CMD = dict(GIM_STATE=0x6604, ZOOM=0x6804, SLOT_LIST=0x3D04, NAME=0x3CC4, POSE=0x3A44,
           ACTIONS=0x3DC4, ADD=0x3944, SET_NAME=0x3A84, RECALL=0x39C4, DELETE=0x3984)


class Query(ctypes.Structure):
    _fields_ = [("unit", ctypes.c_uint8), ("selector", ctypes.c_uint8), ("query", ctypes.c_uint8),
                ("_pad", ctypes.c_uint8), ("size", ctypes.c_uint16), ("_pad2", ctypes.c_uint16),
                ("data", ctypes.POINTER(ctypes.c_uint8))]


def crc16_usb(b):
    c = 0xFFFF
    for x in b:
        c ^= x
        for _ in range(8):
            c = (c >> 1) ^ 0xA001 if c & 1 else c >> 1
    return c ^ 0xFFFF


def frame(flags, seq, cmd, payload=b""):
    f = bytearray(60)
    f[0], f[1] = 0xAA, flags
    struct.pack_into("<HH", f, 2, seq, 0x000C)
    f[8], f[9] = 0x0A, RCV_AI
    struct.pack_into("<H", f, 10, cmd)
    struct.pack_into("<H", f, 6, crc16_usb(bytes(f[0:6]) + b"\0\0" + bytes(f[8:12])))
    if payload:
        assert len(payload) <= 44
        struct.pack_into("<H", f, 12, len(payload))
        struct.pack_into("<H", f, 14, crc16_usb(struct.pack("<H", len(payload)) + b"\0\0" + payload))
        f[16:16 + len(payload)] = payload
    return bytes(f)


class Cam:
    def __init__(self, dev):
        name = os.path.basename(os.path.realpath(dev))
        usb = os.path.realpath(f"/sys/class/video4linux/{name}/device/..")
        ids = tuple(open(f"{usb}/{k}").read().strip() for k in ("idVendor", "idProduct"))
        if ids != TINY3:
            sys.exit(f"{dev} is {ids[0]}:{ids[1]}, not a Tiny 3 ({TINY3[0]}:{TINY3[1]}); refusing")
        self.fd = os.open(dev, os.O_RDWR)
        self.seq = (os.getpid() & 0x3FFF) | 0x4000

    def _xu(self, query, buf):
        arr = (ctypes.c_uint8 * 60).from_buffer(buf)
        fcntl.ioctl(self.fd, UVCIOC_CTRL_QUERY, Query(XU_UNIT, SEL_CMD, query, 0, 60, 0, arr))

    def _next(self):
        self.seq = (self.seq + 1) & 0xFFFF
        return self.seq

    def get(self, cmd, arg=b""):
        seq = self._next()
        self._xu(UVC_SET_CUR, bytearray(frame(FLAG_GET_ARGS if arg else FLAG_GET, seq, cmd, arg)))
        for _ in range(12):
            time.sleep(0.05)
            buf = bytearray(60)
            self._xu(UVC_GET_CUR, buf)
            if buf[0] == 0xAA and struct.unpack_from("<H", buf, 2)[0] == seq \
                    and struct.unpack_from("<H", buf, 10)[0] == cmd:
                if buf[1] != 0x29:
                    return None  # 0x09 ack-without-data or 0x89 error
                n = struct.unpack_from("<H", buf, 12)[0]
                return bytes(buf[16:16 + n]) if 0 < n <= 44 else b""
        return None

    def set(self, cmd, payload):
        self._xu(UVC_SET_CUR, bytearray(frame(FLAG_SET, self._next(), cmd, payload)))


def u32(v):
    return struct.pack("<I", v)


def occupied(cam):
    r = cam.get(CMD["SLOT_LIST"])
    return list(r) if r else []


def pose_record(slot, pan100, tilt100, zoom100):
    # Center's ADD box: centred on 50, half-width 25000 // zoom100 (rounded down; matches all
    # three captured ADDs byte for byte). The camera recomputes the box anyway.
    half = 25000 // zoom100
    return (u32(slot) + struct.pack("<hhhH", pan100, tilt100, 0, zoom100)
            + struct.pack("<hhhh", 50 - half, 50 - half, 50 + half, 50 + half) + u32(0))


def live_pose(cam):
    g = cam.get(CMD["GIM_STATE"])
    z = cam.get(CMD["ZOOM"])
    if not g or len(g) < 12 or not z or len(z) < 4:
        sys.exit("could not read gimbal state / zoom")
    tilt10, pan10 = struct.unpack_from("<hh", g, 8)
    zoom = struct.unpack("<f", z[:4])[0]
    return pan10 * 10, tilt10 * 10, round(zoom * 100)


def show(label, cmd, payload, seq_note="seq assigned at send time"):
    f = frame(FLAG_SET, 0, cmd, payload)
    print(f"  {label}: cmd 0x{cmd:04X}, payload {len(payload)} bytes\n    {payload.hex(' ')}")
    print(f"    full frame (seq=0 shown; {seq_note}): {f[:16 + len(payload)].hex(' ')}")


def describe(cam):
    slots = occupied(cam)
    print(f"occupied slots (0-based): {slots}")
    for s in slots:
        name = cam.get(CMD["NAME"], u32(s))
        pose = cam.get(CMD["POSE"], u32(s) + b"\0")
        acts = cam.get(CMD["ACTIONS"], u32(s))
        print(f"  slot {s}: name={name.decode(errors='replace') if name else None!r}")
        if pose and len(pose) >= 12:
            pan, tilt, roll, zoom = struct.unpack_from("<hhhH", pose, 4)
            print(f"    pose pan {pan / 100:.2f} tilt {tilt / 100:.2f} zoom {zoom / 100:.2f}   raw {pose.hex(' ')}")
        print(f"    settings {acts.hex(' ') if acts else None}")


def main():
    args = [a for a in sys.argv[1:] if a != "--send"]
    send = "--send" in sys.argv[1:]
    if len(args) < 2:
        sys.exit(__doc__)
    cam, action = Cam(args[0]), args[1]
    if action == "read":
        return describe(cam)
    slot = int(args[2]) if len(args) > 2 else -1
    if not 0 <= slot < SLOTS:
        sys.exit(f"SLOT must be 0..{SLOTS - 1} (0-based)")
    occ = occupied(cam)
    print(f"occupied slots (0-based): {occ}")

    if action == "save":
        name = args[3] if len(args) > 3 else ""
        if not (1 <= len(name) <= 16 and name.isascii() and name.isprintable()):
            sys.exit("NAME: 1-16 printable ASCII characters")
        if slot in occ:
            sys.exit(f"slot {slot} is occupied; this tool only saves to an empty slot")
        pan100, tilt100, zoom100 = live_pose(cam)
        print(f"live pose: pan {pan100 / 100:.2f}  tilt {tilt100 / 100:.2f}  zoom {zoom100 / 100:.2f}")
        writes = [("1. PRESET_ADD", CMD["ADD"], pose_record(slot, pan100, tilt100, zoom100)),
                  ("2. set name", CMD["SET_NAME"], u32(slot) + name.encode())]
    elif action in ("recall", "delete"):
        if slot not in occ:
            sys.exit(f"slot {slot} is empty; nothing to {action}")
        writes = ([("1. PRESET_TRIG (recall)", CMD["RECALL"], u32(slot) + struct.pack("<4f", 1, 1, 1, 1))]
                  if action == "recall" else [("1. PRESET_DELETE", CMD["DELETE"], u32(slot))])
    else:
        sys.exit(__doc__)

    print("writes:" if send else "DRY RUN — would send:")
    for label, cmd, payload in writes:
        show(label, cmd, payload)
    if not send:
        print("nothing written (add --send to write)")
        return
    for label, cmd, payload in writes:
        cam.set(cmd, payload)
        print(f"  sent {label}")
        time.sleep(0.05)
    time.sleep(0.5)
    print("after:")
    describe(cam)


if __name__ == "__main__":
    main()
