"""Presets and animations as plain JSON files (git-friendly, hand- or agent-editable)."""
from __future__ import annotations

import json
import os
import tempfile
from typing import Dict, List

from . import animations
from .animations import NAME_RE, SpecError


class Library:
    def __init__(self, root: str):
        self.root = root
        for sub in ("animations", "presets"):
            os.makedirs(os.path.join(root, sub), exist_ok=True)

    def _path(self, kind: str, name: str) -> str:
        if not NAME_RE.match(name or ""):
            raise SpecError("name must be lowercase letters, digits, '-' or '_' (max 40)")
        return os.path.join(self.root, kind, name + ".json")

    def names(self, kind: str) -> List[str]:
        d = os.path.join(self.root, kind)
        return sorted(f[:-5] for f in os.listdir(d) if f.endswith(".json"))

    def get(self, kind: str, name: str) -> dict:
        try:
            with open(self._path(kind, name)) as f:
                spec = json.load(f)
        except FileNotFoundError:
            raise KeyError(f"no {kind[:-1]} named {name!r}")
        spec.setdefault("name", name)
        return spec

    def all(self, kind: str) -> Dict[str, dict]:
        out = {}
        for n in self.names(kind):
            try:
                out[n] = self.get(kind, n)
            except Exception:  # noqa: BLE001 - one bad file must not hide the rest
                continue
        return out

    def save(self, kind: str, name: str, spec: dict) -> dict:
        spec = dict(spec, name=name)
        if kind == "animations":
            animations.load(spec)  # validate before writing
        elif kind == "presets":
            validate_preset(spec)
        path = self._path(kind, name)
        fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
        with os.fdopen(fd, "w") as f:
            json.dump(spec, f, indent=2)
        os.replace(tmp, path)
        return spec

    def delete(self, kind: str, name: str):
        try:
            os.remove(self._path(kind, name))
        except FileNotFoundError:
            raise KeyError(f"no {kind[:-1]} named {name!r}")


def validate_preset(spec: dict):
    """A preset is any combination of: power, color, brightness, effect{mode,speed}, animation."""
    allowed = {"name", "description", "title", "power", "color", "brightness", "effect", "animation", "homekit"}
    extra = set(spec) - allowed
    if extra:
        raise SpecError(f"unknown preset fields: {sorted(extra)}")
    if "color" in spec:
        animations.parse_color(spec["color"])
    if "brightness" in spec and not 1 <= float(spec["brightness"]) <= 100:
        raise SpecError("brightness must be 1-100")
    if sum(k in spec for k in ("color", "effect", "animation")) > 1:
        raise SpecError("a preset can set only one of color / effect / animation")
