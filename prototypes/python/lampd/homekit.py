"""HomeKit bridge (HAP-python) running inside the lampd process.

Exposes a colour Lightbulb, one Switch per built-in effect, and one Switch for every library
animation or preset whose JSON has "homekit": true.
"""
from __future__ import annotations

import colorsys
import logging

from pyhap.accessory import Accessory, Bridge
from pyhap.accessory_driver import AccessoryDriver
from pyhap.const import CATEGORY_LIGHTBULB, CATEGORY_SWITCH

from .engine import Engine

log = logging.getLogger("lampd.homekit")
BUILTIN_SWITCHES = [("Rainbow Fade", 1), ("Rainbow Breathe", 5), ("Rainbow Flash", 9)]


def _later(loop, fn, *args):
    loop.call_soon_threadsafe(lambda: fn(*args))


class Light(Accessory):
    category = CATEGORY_LIGHTBULB

    def __init__(self, driver, engine, name):
        super().__init__(driver, name)
        self.engine, self.loop = engine, driver.loop
        svc = self.add_preload_service("Lightbulb", chars=["On", "Brightness", "Hue", "Saturation"])
        self.c_on = svc.configure_char("On", setter_callback=lambda v: _later(self.loop, engine.set_power, bool(v)))
        self.c_bri = svc.configure_char("Brightness", value=100,
                                        setter_callback=lambda v: _later(self.loop, engine.set_brightness, v))
        self.c_hue = svc.configure_char("Hue", value=0, setter_callback=self._hue)
        self.c_sat = svc.configure_char("Saturation", value=0, setter_callback=self._sat)
        self._hs = [0.0, 0.0]

    def _hue(self, v):
        self._hs[0] = float(v)
        _later(self.loop, self.engine.set_hs, *self._hs)

    def _sat(self, v):
        self._hs[1] = float(v)
        _later(self.loop, self.engine.set_hs, *self._hs)

    def sync(self, s):
        h, sat, _ = colorsys.rgb_to_hsv(*(c / 255 for c in s["rgb"]))
        self._hs = [round(h * 360, 1), round(sat * 100, 1)]
        self.c_on.set_value(s["on"])
        self.c_bri.set_value(s["brightness"])
        self.c_hue.set_value(self._hs[0])
        self.c_sat.set_value(self._hs[1])


class ActionSwitch(Accessory):
    """A tile that turns a built-in effect, animation or preset on or off."""
    category = CATEGORY_SWITCH

    def __init__(self, driver, engine, name, key):
        super().__init__(driver, name)
        self.engine, self.loop, self.key = engine, driver.loop, key
        self.c_on = self.add_preload_service("Switch").configure_char("On", setter_callback=self._set)

    def _set(self, v):
        _later(self.loop, self._apply, bool(v))

    def _apply(self, on: bool):
        kind, _, ident = self.key.partition(":")
        e = self.engine
        if on:
            if kind == "effect":
                e.set_effect(int(ident))
            elif kind == "anim":
                e.play(ident)
            else:
                e.apply_preset(ident)
        elif e.s["active"] == self.key:
            if kind == "preset":
                e.set_power(False)
            else:
                e.set_color(e.s["rgb"])  # leave the effect/animation, keep the colour

    def sync(self, s):
        self.c_on.set_value(bool(s["on"]) and s["active"] == self.key)


class HomeKit:
    def __init__(self, engine: Engine, loop, persist_file: str, port: int = 51826):
        self.engine = engine
        self.driver = AccessoryDriver(port=port, persist_file=persist_file, loop=loop)
        bridge = Bridge(self.driver, "Rainbow Lamp Bridge")
        self.light = Light(self.driver, engine, "Rainbow Lamp")
        bridge.add_accessory(self.light)
        self.switches = []
        items = [(n, f"effect:{m}") for n, m in BUILTIN_SWITCHES]
        for kind, prefix in (("animations", "anim"), ("presets", "preset")):
            for name, spec in engine.library.all(kind).items():
                if spec.get("homekit"):
                    items.append((spec.get("title") or name.replace("-", " ").title(), f"{prefix}:{name}"))
        for title, key in items:
            sw = ActionSwitch(self.driver, engine, title, key)
            self.switches.append(sw)
            bridge.add_accessory(sw)
        self.driver.add_accessory(accessory=bridge)
        engine.listeners.append(self.sync)

    def sync(self):
        s = self.engine.s
        self.light.sync(s)
        for sw in self.switches:
            sw.sync(s)

    async def start(self):
        await self.driver.async_start()
        self.sync()

    async def stop(self):
        await self.driver.async_stop()
