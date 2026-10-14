"""iStrip BLE lamp wire protocol, decoded from the iStrip+ Android app (com.ben.istrips 1.4.3).

Every command is a 16-byte plaintext frame, AES-128-ECB encrypted with a fixed key and
written (without response) to characteristic AC52 of service AC50.

Plaintext layout:  54 52 00 57 | cmd | group | args... (zero padded to 16 bytes)
"""
from __future__ import annotations

import datetime as _dt

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

KEY = bytes.fromhex("34522a5b7a6e492c08090a9d8d2a23f8")
SERVICE = "0000ac50-1212-efde-1523-785fedbeda25"
WRITE_CHAR = "0000ac52-1212-efde-1523-785fedbeda25"
NOTIFY_CHAR = "0000ac51-1212-efde-1523-785fedbeda25"
HEADER = bytes([0x54, 0x52, 0x00, 0x57])

CMD_COLOR, CMD_RHYTHM, CMD_TIMER, CMD_LINE, CMD_SPEED, CMD_BRIGHTNESS = 2, 3, 4, 5, 6, 7

# Built-in effects (command 2, mode = index + 1 in the app's list).
EFFECTS = {
    1: "Fade7", 2: "Fade3", 3: "Breath R", 4: "Strobe R", 5: "Breath7", 6: "Breath3",
    7: "Breath B", 8: "Strobe B", 9: "Flash7", 10: "Flash3", 11: "Breath G", 12: "Strobe G",
}
EFFECT_ALIASES = {
    "rainbow": 1, "rainbow-fade": 1, "fade": 1, "rainbow-breathe": 5, "breathe": 5,
    "rainbow-flash": 9, "flash": 9,
}


def encrypt(frame: bytes) -> bytes:
    assert len(frame) == 16
    e = Cipher(algorithms.AES(KEY), modes.ECB()).encryptor()
    return e.update(frame) + e.finalize()


def decrypt(frame: bytes) -> bytes:
    d = Cipher(algorithms.AES(KEY), modes.ECB()).decryptor()
    return d.update(frame) + d.finalize()


def _byte(v, lo=0, hi=255) -> int:
    return max(lo, min(hi, int(round(v))))


def frame(cmd: int, group: int = 1, *args: int) -> bytes:
    body = bytes([cmd, group, *[_byte(a) for a in args]])
    return HEADER + body + bytes(16 - len(HEADER) - len(body))


def color(r, g, b, brightness=100, speed=100, mode=0, kind=0, group=1) -> bytes:
    """mode 0 = static colour, 1..12 = built-in effect. kind: 0 RGB, 1 white, 2 CCT."""
    return frame(CMD_COLOR, group, mode, r, g, b, _byte(brightness, 0, 100), _byte(speed, 1, 100), kind)


def off(brightness=100, speed=100, group=1) -> bytes:
    return frame(CMD_COLOR, group, 0, 0, 0, 0, _byte(brightness, 0, 100), _byte(speed, 1, 100), 0)


def brightness(value, group=1) -> bytes:
    return frame(CMD_BRIGHTNESS, group, _byte(value, 0, 100))


def speed(value, group=1) -> bytes:
    return frame(CMD_SPEED, group, _byte(value, 1, 100))


def line_sequence(order, group=1) -> bytes:
    """Colour-channel order for strips wired differently (index; mapping not decoded)."""
    return frame(CMD_LINE, group, order)


def rhythm(status, fft=0, sensitivity=0, group=1) -> bytes:
    """Music mode. The phone app computes an FFT value and streams it here (semantics partly decoded)."""
    return frame(CMD_RHYTHM, group, status, fft, sensitivity)


def timer(on_enable, off_enable, on_days, on_hour, on_min, off_days, off_hour, off_min, group=1, now=None) -> bytes:
    """Set the lamp's own on/off timer (runs with no host connected). Also sets the lamp's clock.

    *_days is a weekday bitmask. Bit order (bit0 = Monday) is inferred from the app, not yet verified.
    """
    now = now or _dt.datetime.now()
    weekday = now.isoweekday()  # 1=Mon..7=Sun, as the app encodes it
    flags = (16 if on_enable else 0) | (32 if off_enable else 0) | weekday
    return frame(CMD_TIMER, group, flags, now.hour, now.minute, now.second,
                 on_days, on_hour, on_min, off_days, off_hour, off_min)


def raw(hex_plain: str) -> bytes:
    """Validated raw plaintext frame. Only the control commands 2..7 are allowed."""
    data = bytes.fromhex(hex_plain.replace(" ", ""))
    if len(data) != 16 or data[:4] != HEADER or data[4] not in (2, 3, 4, 5, 6, 7):
        raise ValueError("raw frame must be 16 bytes: 54 52 00 57 <cmd 2-7> ...")
    return data


# Self-describing list of functions, served at GET /api/ops and used by POST /api/raw.
OPS = {
    "color": dict(fn=color, doc="Static colour (or built-in effect via mode). brightness/speed 0-100.",
                  params="r g b [brightness=100] [speed=100] [mode=0] [kind=0]"),
    "off": dict(fn=off, doc="Turn the lamp off (colour command with RGB 0).", params="[brightness] [speed]"),
    "brightness": dict(fn=brightness, doc="Set brightness 0-100.", params="value"),
    "speed": dict(fn=speed, doc="Set effect speed 1-100.", params="value"),
    "line_sequence": dict(fn=line_sequence, doc="Colour-channel order index (mapping not decoded).", params="order"),
    "rhythm": dict(fn=rhythm, doc="Music mode frame (partly decoded).", params="status [fft] [sensitivity]"),
    "timer": dict(fn=timer, doc="Lamp-side on/off timer; UNVERIFIED weekday bit order.",
                  params="on_enable off_enable on_days on_hour on_min off_days off_hour off_min"),
}


def build(op: str, **kwargs) -> bytes:
    if op not in OPS:
        raise ValueError(f"unknown op {op!r}; known: {', '.join(OPS)}")
    return OPS[op]["fn"](**kwargs)
