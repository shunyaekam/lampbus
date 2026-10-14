"""Declarative animations: validated JSON in, (rgb, intensity) per timestamp out.

No code from an animation file is ever executed, so an agent can author them safely.

Spec (JSON):
  {"name": "...", "description": "...", "fps": 20, "homekit": false,
   "type": "keyframes", "duration": 60, "loop": true, "space": "rgb"|"hsv", "then": "hold"|"off",
   "keys": [{"t": 0, "color": "#ff8800" | [r,g,b] | {"h":0-360,"s":0-100,"v":0-100},
             "level": 0-100, "ease": "linear"|"smooth"|"hold"}, ...]}
  {"type": "gen", "gen": "rainbow"|"breathe"|"candle"|"aurora", "params": {...}, ...}

`level` is intensity as a percentage of the master brightness (default 100).
"""
from __future__ import annotations

import colorsys
import math
import re
from typing import List, Optional, Tuple

MAX_KEYS, MAX_DURATION, MAX_FPS = 500, 86400.0, 40.0
NAME_RE = re.compile(r"^[a-z0-9][a-z0-9_-]{0,39}$")

RGB = Tuple[float, float, float]
NAMED = {
    "red": (255, 0, 0), "orange": (255, 110, 0), "amber": (255, 150, 20), "yellow": (255, 210, 0),
    "green": (0, 255, 0), "cyan": (0, 255, 255), "blue": (0, 0, 255), "purple": (140, 0, 255),
    "magenta": (255, 0, 200), "pink": (255, 80, 140), "white": (255, 255, 255),
    "warm": (255, 147, 41), "candle": (255, 120, 20), "cool": (200, 225, 255),
}


class SpecError(ValueError):
    pass


def parse_color(c) -> RGB:
    """Accepts '#rrggbb', a name, [r,g,b], or {"h","s","v"}."""
    try:
        if isinstance(c, str):
            if c.lower() in NAMED:
                return tuple(float(x) for x in NAMED[c.lower()])
            m = re.fullmatch(r"#?([0-9a-fA-F]{6})", c.strip())
            if not m:
                raise SpecError(f"bad colour {c!r}")
            v = int(m.group(1), 16)
            return float(v >> 16), float((v >> 8) & 255), float(v & 255)
        if isinstance(c, (list, tuple)) and len(c) == 3:
            return tuple(max(0.0, min(255.0, float(x))) for x in c)
        if isinstance(c, dict):
            r, g, b = colorsys.hsv_to_rgb((float(c["h"]) % 360) / 360, float(c.get("s", 100)) / 100,
                                          float(c.get("v", 100)) / 100)
            return r * 255, g * 255, b * 255
    except (KeyError, TypeError, ValueError) as e:
        raise SpecError(f"bad colour {c!r}: {e}")
    raise SpecError(f"bad colour {c!r}")


def _lerp(a: float, b: float, u: float) -> float:
    return a + (b - a) * u


def _ease(kind: str, u: float) -> float:
    if kind == "hold":
        return 0.0
    if kind == "smooth":
        return u * u * (3 - 2 * u)
    return u


def _mix(a: RGB, b: RGB, u: float, space: str) -> RGB:
    if space == "hsv":
        ha, sa, va = colorsys.rgb_to_hsv(*(x / 255 for x in a))
        hb, sb, vb = colorsys.rgb_to_hsv(*(x / 255 for x in b))
        dh = ((hb - ha + 0.5) % 1.0) - 0.5  # shortest way round the hue wheel
        r, g, bl = colorsys.hsv_to_rgb((ha + dh * u) % 1.0, _lerp(sa, sb, u), _lerp(va, vb, u))
        return r * 255, g * 255, bl * 255
    return tuple(_lerp(x, y, u) for x, y in zip(a, b))


def _noise(t: float, seed: float = 0.0) -> float:
    """Smooth pseudo-noise in [-1, 1] from incommensurate sines (deterministic, dependency-free)."""
    return (math.sin(t * 1.00 + seed) * 0.5 + math.sin(t * 2.31 + seed * 1.7) * 0.3
            + math.sin(t * 4.77 + seed * 2.9) * 0.2)


class Animation:
    name = ""
    description = ""
    fps = 20.0
    duration: Optional[float] = None  # None = endless
    loop = True
    then = "hold"

    def sample(self, t: float) -> Tuple[RGB, float]:  # -> (rgb 0-255, level 0-1)
        raise NotImplementedError


class Keyframes(Animation):
    def __init__(self, spec: dict):
        self.duration = float(spec.get("duration", 0))
        if not 0 < self.duration <= MAX_DURATION:
            raise SpecError(f"duration must be in (0, {MAX_DURATION}] seconds")
        self.loop = bool(spec.get("loop", False))
        self.space = spec.get("space", "rgb")
        if self.space not in ("rgb", "hsv"):
            raise SpecError("space must be 'rgb' or 'hsv'")
        keys = spec.get("keys")
        if not isinstance(keys, list) or not 2 <= len(keys) <= MAX_KEYS:
            raise SpecError(f"keys must be a list of 2..{MAX_KEYS}")
        parsed = []
        for k in keys:
            try:
                t = float(k["t"])
            except (KeyError, TypeError, ValueError):
                raise SpecError("each key needs a numeric 't'")
            if not 0 <= t <= self.duration:
                raise SpecError(f"key t={t} outside 0..duration")
            ease = k.get("ease", "linear")
            if ease not in ("linear", "smooth", "hold"):
                raise SpecError(f"bad ease {ease!r}")
            level = float(k.get("level", 100))
            if not 0 <= level <= 100:
                raise SpecError("level must be 0-100")
            parsed.append((t, parse_color(k.get("color", "#ffffff")), level / 100, ease))
        parsed.sort(key=lambda k: k[0])
        if self.loop and parsed[-1][0] < self.duration:
            parsed.append((self.duration, parsed[0][1], parsed[0][2], "linear"))
        self.keys: List = parsed

    def sample(self, t):
        if self.loop:
            t %= self.duration
        keys = self.keys
        if t <= keys[0][0]:
            return keys[0][1], keys[0][2]
        for a, b in zip(keys, keys[1:]):
            if t <= b[0]:
                span = b[0] - a[0]
                u = _ease(a[3], (t - a[0]) / span) if span > 0 else 1.0
                return _mix(a[1], b[1], u, self.space), _lerp(a[2], b[2], u)
        return keys[-1][1], keys[-1][2]


class Generator(Animation):
    GENS = ("rainbow", "breathe", "candle", "aurora")

    def __init__(self, spec: dict):
        self.gen = spec.get("gen")
        if self.gen not in self.GENS:
            raise SpecError(f"gen must be one of {self.GENS}")
        p = spec.get("params", {}) or {}
        d = spec.get("duration")
        self.duration = float(d) if d is not None else None
        if self.duration is not None and not 0 < self.duration <= MAX_DURATION:
            raise SpecError("duration out of range")
        self.loop = False if self.duration else True
        try:
            if self.gen == "rainbow":
                self.period = float(p.get("period", 30)); self.sat = float(p.get("saturation", 100)) / 100
            elif self.gen == "breathe":
                self.rgb = parse_color(p.get("color", "warm")); self.period = float(p.get("period", 6))
                self.lo = float(p.get("min", 15)) / 100; self.hi = float(p.get("max", 100)) / 100
            elif self.gen == "candle":
                self.rgb = parse_color(p.get("color", "candle")); self.depth = float(p.get("flicker", 35)) / 100
            elif self.gen == "aurora":
                pal = p.get("palette", ["#00ff90", "#0090ff", "#8a2be2"])
                if not isinstance(pal, list) or not 2 <= len(pal) <= 12:
                    raise SpecError("palette must be a list of 2..12 colours")
                self.palette = [parse_color(c) for c in pal]
                self.rate = float(p.get("speed", 0.15))
        except (TypeError, ValueError) as e:
            raise SpecError(f"bad params: {e}")
        if getattr(self, "period", 1) <= 0:
            raise SpecError("period must be > 0")

    def sample(self, t):
        g = self.gen
        if g == "rainbow":
            r, gg, b = colorsys.hsv_to_rgb((t / self.period) % 1.0, self.sat, 1.0)
            return (r * 255, gg * 255, b * 255), 1.0
        if g == "breathe":
            k = (1 - math.cos(2 * math.pi * t / self.period)) / 2
            return self.rgb, _lerp(self.lo, self.hi, k)
        if g == "candle":
            n = (_noise(t * 3.0, 1.3) + _noise(t * 9.0, 4.1) * 0.5) / 1.5  # -1..1
            level = max(0.05, min(1.0, 0.72 + n * self.depth))
            r, gg, b = self.rgb
            cool = (1 - level) * 0.35  # dimmer flame drifts redder
            return (r, gg * (1 - cool), b * (1 - cool)), level
        # aurora: drift through the palette with slow noise, breathing intensity
        pos = (0.5 + 0.5 * _noise(t * self.rate, 2.2)) * (len(self.palette) - 1)
        i = min(int(pos), len(self.palette) - 2)
        rgb = _mix(self.palette[i], self.palette[i + 1], pos - i, "hsv")
        return rgb, 0.55 + 0.45 * (0.5 + 0.5 * _noise(t * self.rate * 1.7, 7.7))


def load(spec: dict) -> Animation:
    """Validate a spec dict and build an Animation. Raises SpecError."""
    if not isinstance(spec, dict):
        raise SpecError("animation spec must be an object")
    kind = spec.get("type")
    anim = Keyframes(spec) if kind == "keyframes" else Generator(spec) if kind == "gen" else None
    if anim is None:
        raise SpecError("type must be 'keyframes' or 'gen'")
    fps = float(spec.get("fps", 20))
    if not 0.1 <= fps <= MAX_FPS:
        raise SpecError(f"fps must be 0.1-{MAX_FPS}")
    anim.fps = fps
    anim.then = spec.get("then", "hold")
    if anim.then not in ("hold", "off"):
        raise SpecError("then must be 'hold' or 'off'")
    anim.name = str(spec.get("name", ""))
    anim.description = str(spec.get("description", ""))
    return anim
