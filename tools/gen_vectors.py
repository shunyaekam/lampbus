#!/usr/bin/env python3
"""Generate conformance vectors for definitions/*.json from INDEPENDENT oracles.

Each oracle re-implements the frame construction of an upstream open-source project (or, for the iStrip lamp,
the hardware-verified prototype) literally, with no shared code with the Rust interpreter. The Rust conformance
test then checks that the data-driven definitions reproduce these bytes exactly.

Run with the prototype's venv (needs `cryptography` for the iStrip AES oracle):
    .venv/bin/python tools/gen_vectors.py
"""
import json, os, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "prototypes", "python"))
from lampd import protocol as istrip  # hardware-verified oracle for the iStrip lamp

COLORS = [(0, 0, 0), (255, 255, 255), (255, 0, 128), (1, 2, 3), (18, 52, 86), (255, 147, 41)]


def hx(b): return bytes(b).hex()


# ---- iStrip: prototype that has driven a real lamp -------------------------------------------------------------
def istrip_vectors():
    v = []
    def add(cmd, args, plain):
        v.append({"command": cmd, "args": args, "plain": hx(plain), "wire": hx(istrip.encrypt(plain))})
    for r, g, b in COLORS:
        add("color", {"r": r, "g": g, "b": b}, istrip.color(r, g, b))
    add("color", {"r": 255, "g": 0, "b": 0, "brightness": 30, "speed": 50}, istrip.color(255, 0, 0, 30, 50))
    add("color", {"r": 9, "g": 8, "b": 7, "brightness": 0, "speed": 1, "group": 3}, istrip.color(9, 8, 7, 0, 1, group=3))
    add("off", {}, istrip.off())
    add("off", {"brightness": 40, "speed": 20}, istrip.off(40, 20))
    for mode in (1, 5, 9, 12):
        add("effect", {"mode": mode}, istrip.color(0, 0, 0, 100, 100, mode=mode))
    add("effect", {"mode": 3, "brightness": 60, "speed": 10}, istrip.color(0, 0, 0, 60, 10, mode=3))
    for level in (0, 1, 50, 100):
        add("brightness", {"level": level}, istrip.brightness(level))
    for value in (1, 30, 100):
        add("speed", {"value": value}, istrip.speed(value))
    return v


# ---- ELK-BLEDOM: FergusInLondon/ELK-BLEDOM device/commands.go and TheSylex/... src/device.rs ----------------------
def elk_vectors():
    v = []
    def add(cmd, args, plain): v.append({"command": cmd, "args": args, "plain": hx(plain)})
    add("power_on", {}, bytes([0x7E, 0x00, 0x04, 0xF0, 0x00, 0x01, 0xFF, 0x00, 0xEF]))
    add("power_off", {}, bytes([0x7E, 0x00, 0x04, 0x00, 0x00, 0x00, 0xFF, 0x00, 0xEF]))
    for r, g, b in COLORS:
        add("color", {"r": r, "g": g, "b": b}, bytes([0x7E, 0x00, 0x05, 0x03, r, g, b, 0x00, 0xEF]))
    for level in (0, 1, 50, 100):
        add("brightness", {"level": level}, bytes([0x7E, 0x00, 0x01, level, 0x00, 0x00, 0x00, 0x00, 0xEF]))
    return v


# ---- Triones: lightinglibs/flux_led protocol.py (original protocol messages) -----------------------------------------
def triones_vectors():
    v = []
    def add(cmd, args, plain): v.append({"command": cmd, "args": args, "plain": hx(plain)})
    add("power_on", {}, bytes([0xCC, 0x23, 0x33]))
    add("power_off", {}, bytes([0xCC, 0x24, 0x33]))
    for r, g, b in COLORS:
        add("color", {"r": r, "g": g, "b": b}, bytes([0x56, r, g, b, 0x00, 0xF0, 0xAA]))
    return v


# ---- Govee: Beshelmek/govee_ble_lights light.py _prepareSinglePacketData ---------------------------------------------
def govee_frame(cmd, payload):
    frame = bytes([0x33, cmd]) + bytes(payload)
    frame += bytes([0] * (19 - len(frame)))      # pad to 19 bytes (plus checksum)
    checksum = 0
    for b in frame:                              # XOR of all data bytes
        checksum ^= b
    return frame + bytes([checksum & 0xFF])


def govee_vectors():
    v = []
    def add(cmd, args, plain): v.append({"command": cmd, "args": args, "plain": hx(plain)})
    add("power_on", {}, govee_frame(0x01, [0x1]))
    add("power_off", {}, govee_frame(0x01, [0x0]))
    for level in (0, 1, 128, 255):
        add("brightness", {"level": level}, govee_frame(0x04, [level]))
    for r, g, b in COLORS:
        add("color", {"r": r, "g": g, "b": b}, govee_frame(0x05, [0x02, r, g, b]))
    return v


for name, fn in [("istrip-ssl", istrip_vectors), ("elk-bledom", elk_vectors),
                 ("triones", triones_vectors), ("govee-h61", govee_vectors)]:
    out = os.path.join(ROOT, "definitions", f"{name}.vectors.json")
    vecs = fn()
    with open(out, "w") as f:
        json.dump(vecs, f, indent=1)
        f.write("\n")
    print(f"{name:12} {len(vecs):3} vectors -> {os.path.relpath(out, ROOT)}")
