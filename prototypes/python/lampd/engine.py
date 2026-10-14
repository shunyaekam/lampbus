"""Lamp state + source arbitration.

Priority, highest first: live stream (screen/music, expires 2 s after its last frame) >
animation > built-in effect > static colour. Everything ends up as plaintext frames handed to the
Device. Single-threaded: call from the asyncio loop.
"""
from __future__ import annotations

import colorsys
import json
import logging
import os
import time
from typing import Callable, List, Optional

import asyncio

from . import animations, protocol
from .animations import SpecError, parse_color
from .device import Device
from .library import Library

log = logging.getLogger("lampd.engine")
STREAM_TIMEOUT = 2.0


class Engine:
    def __init__(self, device: Device, library: Library, state_path: str):
        self.device = device
        self.library = library
        self.state_path = state_path
        self.s = {"on": False, "rgb": [255, 255, 255], "brightness": 100, "effect": None,
                  "speed": 100, "animation": None, "active": None}
        self._anim: Optional[animations.Animation] = None
        self._ticker: Optional[asyncio.Task] = None
        self._stream_until = 0.0
        self._stream_timer = None
        self._save_handle = None
        self.listeners: List[Callable[[], None]] = []
        self._load_state()
        device.on_connection_change = lambda up: self._notify(save=False)

    # -- state ------------------------------------------------------------------------------
    def _load_state(self):
        try:
            with open(self.state_path) as f:
                saved = json.load(f)
            self.s.update({k: saved[k] for k in self.s if k in saved})
            if self.s["animation"]:
                self._anim = animations.load(self.library.get("animations", self.s["animation"]))
        except FileNotFoundError:
            pass
        except Exception as e:  # noqa: BLE001
            log.warning("could not restore state: %s", e)
            self.s.update(animation=None, active=None)

    def _notify(self, save=True):
        if save and not self._save_handle:
            self._save_handle = asyncio.get_event_loop().call_later(1.0, self._save_state)
        for fn in list(self.listeners):
            try:
                fn()
            except Exception:  # noqa: BLE001
                log.exception("listener failed")

    def _save_state(self):
        self._save_handle = None
        tmp = self.state_path + ".tmp"
        os.makedirs(os.path.dirname(self.state_path), exist_ok=True)
        with open(tmp, "w") as f:
            json.dump(self.s, f)
        os.replace(tmp, self.state_path)

    @property
    def streaming(self) -> bool:
        return time.monotonic() < self._stream_until

    def snapshot(self) -> dict:
        s = dict(self.s)
        s["source"] = ("off" if not s["on"] else "stream" if self.streaming else "animation" if s["animation"]
                       else "effect" if s["effect"] else "color")
        s["connected"] = self.device.connected
        s["last_error"] = self.device.last_error
        return s

    # -- control ----------------------------------------------------------------------------
    def set_power(self, on: bool):
        self.s["on"] = bool(on)
        self.refresh()

    def set_color(self, color, brightness: Optional[float] = None, power: bool = True):
        self.s["rgb"] = [round(c) for c in parse_color(color)]
        if brightness is not None:
            self.s["brightness"] = max(1, min(100, round(brightness)))
        self._clear_modes()
        if power:
            self.s["on"] = True
        self.refresh()

    def set_hs(self, hue: float, sat: float):
        """HomeKit-style hue 0-360 / saturation 0-100 at full value."""
        r, g, b = colorsys.hsv_to_rgb((hue % 360) / 360, max(0, min(100, sat)) / 100, 1.0)
        self.s["rgb"] = [round(r * 255), round(g * 255), round(b * 255)]
        self._clear_modes()
        self.refresh()

    def set_brightness(self, value: float):
        self.s["brightness"] = max(1, min(100, round(value)))
        self.refresh()

    def set_effect(self, mode, speed: Optional[float] = None, brightness: Optional[float] = None):
        if isinstance(mode, str):
            mode = protocol.EFFECT_ALIASES.get(mode.lower(), mode)
            if isinstance(mode, str):
                names = {v.lower(): k for k, v in protocol.EFFECTS.items()}
                if mode.lower() not in names:
                    raise SpecError(f"unknown effect {mode!r}")
                mode = names[mode.lower()]
        if mode not in protocol.EFFECTS:
            raise SpecError("effect mode must be 1-12")
        self._stop_anim()
        self.s.update(effect=int(mode), active=f"effect:{int(mode)}", on=True)
        if speed is not None:
            self.s["speed"] = max(1, min(100, round(speed)))
        if brightness is not None:
            self.s["brightness"] = max(1, min(100, round(brightness)))
        self.refresh()

    def play(self, name: Optional[str] = None, spec: Optional[dict] = None, brightness: Optional[float] = None):
        spec = spec if spec is not None else self.library.get("animations", name)
        anim = animations.load(spec)
        self._stop_anim()
        self._anim = anim
        self.s.update(animation=name or "(inline)", effect=None, on=True,
                      active=f"anim:{name}" if name else None)
        if brightness is not None:
            self.s["brightness"] = max(1, min(100, round(brightness)))
        self.refresh()

    def stop_animation(self):
        self._stop_anim()
        self.s["active"] = None
        self.refresh()

    def apply_preset(self, name: str):
        spec = self.library.get("presets", name)
        if "animation" in spec:
            self.play(spec["animation"], brightness=spec.get("brightness"))
        elif "effect" in spec:
            e = spec["effect"]
            self.set_effect(e.get("mode", 1), e.get("speed"), spec.get("brightness"))
        elif "color" in spec:
            self.set_color(spec["color"], spec.get("brightness"), power=spec.get("power", True))
        else:
            if "brightness" in spec:
                self.s["brightness"] = max(1, min(100, round(spec["brightness"])))
            if "power" in spec:
                self.s["on"] = bool(spec["power"])
            self.refresh()
        self.s["active"] = f"preset:{name}"
        self._notify()

    def push_stream(self, r: float, g: float, b: float, brightness: float = 100):
        """One frame from an external live source (screen/music). Overrides everything else."""
        if not self.s["on"]:
            return
        if not self.streaming:
            self.device.hold(+1)
            self._notify(save=False)
        self._stream_until = time.monotonic() + STREAM_TIMEOUT
        if self._stream_timer:
            self._stream_timer.cancel()
        self._stream_timer = asyncio.get_event_loop().call_later(STREAM_TIMEOUT + 0.05, self._stream_expired)
        level = max(1, min(100, round(brightness * self.s["brightness"] / 100)))
        self.device.submit(protocol.color(r, g, b, level))

    def _stream_expired(self):
        if not self.streaming:
            self.device.hold(-1)
            self.refresh()

    # -- rendering --------------------------------------------------------------------------
    def _clear_modes(self):
        self._stop_anim()
        self.s.update(effect=None, active=None)

    def _stop_anim(self):
        if self._ticker:
            self._ticker.cancel()
            self._ticker = None
        self._anim = None
        self.s["animation"] = None

    def refresh(self):
        """Recompute and send what the lamp should show now, then tell listeners."""
        s = self.s
        if self.streaming and s["on"]:
            pass  # stream frames drive the lamp
        elif not s["on"]:
            if self._ticker:  # pause the animation (it restarts on power-on) and free the connection
                self._ticker.cancel()
                self._ticker = None
            self.device.submit(protocol.off())
        elif self._anim:
            if not self._ticker:
                self._ticker = asyncio.ensure_future(self._run_animation(self._anim))
        elif s["effect"]:
            self.device.submit(protocol.color(0, 0, 0, s["brightness"], s["speed"], mode=s["effect"]))
        else:
            self.device.submit(protocol.color(*s["rgb"], s["brightness"]))
        self._notify()

    async def _run_animation(self, anim: animations.Animation):
        loop = asyncio.get_event_loop()
        t0, period = loop.time(), 1.0 / anim.fps
        n = 0
        self.device.hold(+1)
        try:
            while True:
                t = loop.time() - t0
                finished = anim.duration is not None and not anim.loop and t >= anim.duration
                (r, g, b), level = anim.sample(min(t, anim.duration) if finished else t)
                if self.s["on"] and not self.streaming:
                    bri = max(1, round(self.s["brightness"] * level))
                    if finished and anim.then == "off":
                        self.s["on"] = False
                        self.device.submit(protocol.off())
                    else:
                        self.device.submit(protocol.color(r, g, b, bri))
                if finished:
                    self.s.update(rgb=[round(r), round(g), round(b)], animation=None, active=None)
                    self._anim = None
                    self._ticker = None
                    self._notify()
                    return
                n += 1
                await asyncio.sleep(max(0.0, t0 + n * period - loop.time()))
        finally:
            self.device.hold(-1)
