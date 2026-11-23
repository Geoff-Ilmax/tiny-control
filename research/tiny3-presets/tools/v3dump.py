#!/usr/bin/env python3
"""Throwaway research tool: list OBSBOT XU traffic in a QEMU usb-host pcap.

Reads a LINKTYPE_USB_LINUX_MMAPPED (220) pcap, keeps the completed control
transfers to the vendor extension unit (bUnitID 2), and prints one line per
transfer. Selector 2 payloads are decoded as V3 frames (see src/frame.rs).

usage: v3dump.py CAPTURE.pcap [--src REPO/src/device.rs] [--from HH:MM:SS] [--to HH:MM:SS]
                              [--sel 2,4,6] [--no-status]
"""
import argparse, collections, datetime, re, struct, sys

XU_UNIT = 2
SET_CUR, GET_CUR = 0x01, 0x81


def names_from(src):
    names = {}
    try:
        for m in re.finditer(r"const (CMD_\w+): u16 = (0x[0-9A-Fa-f]+);", open(src).read()):
            names.setdefault(int(m.group(2), 16), m.group(1)[4:])
    except OSError:
        pass
    return names


def packets(path):
    d = open(path, "rb").read()
    if struct.unpack("<I", d[:4])[0] != 0xA1B2C3D4 or struct.unpack("<I", d[20:24])[0] != 220:
        sys.exit("not a little-endian usbmon-mmapped pcap")
    o = 24
    while o + 16 <= len(d):
        ts, tu, incl, _ = struct.unpack("<IIII", d[o:o + 16])
        yield ts + tu / 1e6, d[o + 16:o + 16 + incl]
        o += 16 + incl


def transfers(path):
    """Pair Submit/Complete of control transfers by URB id."""
    pending = {}
    for t, p in packets(path):
        if len(p) < 64 or p[9] != 2:  # control only
            continue
        urb, typ = struct.unpack("<Q", p[:8])[0], chr(p[8])
        data = p[64:]
        if typ == "S":
            setup = p[40:48]
            pending[urb] = (t, setup, data)
        elif typ == "C" and urb in pending:
            t0, setup, sdata = pending.pop(urb)
            status = struct.unpack("<i", p[28:32])[0]
            yield t0, setup, sdata if setup[0] & 0x80 == 0 else data, status


def v3(b):
    if len(b) < 12 or b[0] != 0xAA:
        return None
    flags, seq, recv, cmd = b[1], struct.unpack("<H", b[2:4])[0], b[9], struct.unpack("<H", b[10:12])[0]
    payload = b""
    # 0x20 = nested payload segment present (0x21 GET-with-args, 0x25 SET, 0x29 reply); 0x04 kept for older notes.
    if len(b) >= 16 and (flags & 0x24 or flags in (0x89,)):
        n = struct.unpack("<H", b[12:14])[0]
        payload = b[16:16 + n] if 0 < n <= 44 else b""
    return flags, seq, b[8], recv, cmd, payload


def hhmmss(s):
    return datetime.datetime.strptime(s, "%H:%M:%S").time() if s else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("pcap")
    ap.add_argument("--src", default="src/device.rs")
    ap.add_argument("--from", dest="t_from")
    ap.add_argument("--to", dest="t_to")
    ap.add_argument("--sel", default="2,4,6,12,13")
    ap.add_argument("--no-status", action="store_true", help="hide the polled status reads (sel 4/6 GET)")
    ap.add_argument("--summary", action="store_true", help="count V3 commands instead of listing")
    a = ap.parse_args()
    names, sels = names_from(a.src), {int(x) for x in a.sel.split(",")}
    lo, hi = hhmmss(a.t_from), hhmmss(a.t_to)
    counts = collections.Counter()
    for t, setup, data, status in transfers(a.pcap):
        rt, req, sel, unit = setup[0], setup[1], setup[3], setup[5]
        if unit != XU_UNIT or rt & 0x60 != 0x20 or req not in (SET_CUR, GET_CUR) or sel not in sels:
            continue
        when = datetime.datetime.fromtimestamp(t)
        if (lo and when.time() < lo) or (hi and when.time() > hi):
            continue
        op = "SET" if req == SET_CUR else "GET"
        if a.no_status and op == "GET" and sel != 2:
            continue
        stamp = when.strftime("%H:%M:%S.%f")[:-3]
        err = f" status={status}" if status else ""
        f = v3(data) if sel == 2 else None
        if f:
            flags, seq, snd, recv, cmd, payload = f
            name = names.get(cmd, "?")
            if a.summary:
                counts[(op, hex(flags), recv, hex(cmd), name)] += 1
                continue
            print(f"{stamp} {op} sel2 fl={flags:02x} seq={seq:5d} {snd:02x}->{recv:02x} "
                  f"cmd=0x{cmd:04X} {name:<28} {payload.hex(' ')}{err}")
        elif not a.summary:
            body = data.rstrip(b"\0")
            print(f"{stamp} {op} sel{sel} {body.hex(' ')}{err}")
    for k, v in sorted(counts.items(), key=lambda kv: -kv[1]):
        print(f"{v:5d}  {k[0]} flags={k[1]} recv={k[2]:#04x} cmd={k[3]} {k[4]}")


if __name__ == "__main__":
    main()
