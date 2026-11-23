#!/usr/bin/env python3
"""Throwaway research tool: send ONE read-only V3 GET to an OBSBOT camera.

Only GET frames (flags 0x01 / 0x21) for an allowlist of read commands can be
sent; there is no way to send a SET from here. Frame format as in src/frame.rs.

usage: v3get.py /dev/videoN CMD [ARGHEX]      e.g.  v3get.py /dev/video2 0x3A44 0000000000
"""
import ctypes, fcntl, os, struct, sys, time

READ_ONLY = {
    0x3A44: "preset position (GIMBAL_PRESET_ID_VALUE)",
    0x3B04: "preset name (GIMBAL_PRESET_ID_NAME)",
    0x3B44: "preset list (GIMBAL_PRESET_LIST)",
    0x3CC4: "preset name (Center)",
    0x3D04: "occupied slot list (Center)",
    0x3DC4: "preset settings (PRESETS_ACTIONS)",
    0x1D04: "preset OPA",
    0x1C44: "initial-position OPA",
    0x3884: "power-on pose",
    0x3E84: "power-on settings",
    0x6604: "gimbal state",
    0x6804: "zoom ratio",
}
RCV_AI, XU_UNIT, SEL_CMD = 0x04, 2, 2
UVC_SET_CUR, UVC_GET_CUR = 0x01, 0x81
UVCIOC_CTRL_QUERY = 0xC0107521


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


def build_get(seq, cmd, arg):
    f = bytearray(60)
    f[0], f[1] = 0xAA, 0x21 if arg else 0x01
    struct.pack_into("<HH", f, 2, seq, 0x000C)
    f[8], f[9] = 0x0A, RCV_AI
    struct.pack_into("<H", f, 10, cmd)
    struct.pack_into("<H", f, 6, crc16_usb(bytes(f[0:6]) + b"\0\0" + bytes(f[8:12])))
    if arg:
        struct.pack_into("<H", f, 12, len(arg))
        struct.pack_into("<H", f, 14, crc16_usb(struct.pack("<H", len(arg)) + b"\0\0" + arg))
        f[16:16 + len(arg)] = arg
    return bytes(f)


def xu(fd, query, buf):
    arr = (ctypes.c_uint8 * 60).from_buffer(buf)
    q = Query(XU_UNIT, SEL_CMD, query, 0, 60, 0, arr)
    fcntl.ioctl(fd, UVCIOC_CTRL_QUERY, q)


def main():
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    dev, cmd = sys.argv[1], int(sys.argv[2], 16)
    arg = bytes.fromhex(sys.argv[3]) if len(sys.argv) == 4 else b""
    if cmd not in READ_ONLY:
        sys.exit(f"0x{cmd:04X} is not on the read-only allowlist")
    if len(arg) > 44:
        sys.exit("argument too long")
    seq = (os.getpid() & 0x7FFF) | 0x4000
    fd = os.open(dev, os.O_RDWR)
    try:
        xu(fd, UVC_SET_CUR, bytearray(build_get(seq, cmd, arg)))
        for _ in range(12):
            time.sleep(0.05)
            buf = bytearray(60)
            xu(fd, UVC_GET_CUR, buf)
            if buf[0] == 0xAA and struct.unpack_from("<HH", buf, 2)[0] == seq \
                    and struct.unpack_from("<H", buf, 10)[0] == cmd:
                flags = buf[1]
                n = struct.unpack_from("<H", buf, 12)[0]
                payload = bytes(buf[16:16 + n]) if 0 < n <= 44 else b""
                print(f"0x{cmd:04X} {READ_ONLY[cmd]} req[{arg.hex(' ')}] -> flags={flags:02x} "
                      f"len={n}: {payload.hex(' ')}")
                return
        print(f"0x{cmd:04X} req[{arg.hex(' ')}] -> no reply")
    finally:
        os.close(fd)


if __name__ == "__main__":
    main()
