"""HTTP + WebSocket API. JSON in/out, Bearer-token auth (header or ?token=)."""
from __future__ import annotations

import asyncio
import hmac
import json
import logging
import os

from aiohttp import WSMsgType, web

from . import protocol
from .animations import SpecError
from .engine import Engine

log = logging.getLogger("lampd.api")
WEB_DIR = os.path.join(os.path.dirname(__file__), "web")


def make_app(engine: Engine, token: str) -> web.Application:
    @web.middleware
    async def guard(request: web.Request, handler):
        if request.path.startswith("/api/"):
            given = request.headers.get("Authorization", "").removeprefix("Bearer ") or request.query.get("token", "")
            if not hmac.compare_digest(given.encode(), token.encode()):
                return web.json_response({"error": "unauthorized"}, status=401)
        try:
            return await handler(request)
        except web.HTTPException:
            raise
        except (SpecError, ValueError, TypeError) as e:
            return web.json_response({"error": str(e)}, status=400)
        except KeyError as e:
            return web.json_response({"error": str(e.args[0]) if e.args else "not found"}, status=404)

    async def body(request) -> dict:
        if not request.can_read_body:
            return {}
        data = await request.json()
        if not isinstance(data, dict):
            raise ValueError("body must be a JSON object")
        return data

    def state():
        return web.json_response(engine.snapshot())

    routes = web.RouteTableDef()

    @routes.get("/api/state")
    async def get_state(_):
        return state()

    @routes.post("/api/power")
    async def power(r):
        d = await body(r)
        engine.set_power(d["on"] if "on" in d else not engine.s["on"])
        return state()

    @routes.post("/api/color")
    async def color(r):
        d = await body(r)
        if "color" not in d and "hue" in d:
            engine.set_hs(d["hue"], d.get("saturation", 100))
            if "brightness" in d:
                engine.set_brightness(d["brightness"])
            engine.s["on"] = True
            engine.refresh()
        else:
            engine.set_color(d["color"], d.get("brightness"))
        return state()

    @routes.post("/api/brightness")
    async def brightness(r):
        engine.set_brightness((await body(r))["value"])
        return state()

    @routes.post("/api/effect")
    async def effect(r):
        d = await body(r)
        engine.set_effect(d["effect"], d.get("speed"), d.get("brightness"))
        return state()

    @routes.get("/api/effects")
    async def effects(_):
        return web.json_response({"effects": {str(k): v for k, v in protocol.EFFECTS.items()},
                                  "aliases": protocol.EFFECT_ALIASES})

    # animations ------------------------------------------------------------------------------
    @routes.get("/api/animations")
    async def animations_list(_):
        return web.json_response(engine.library.all("animations"))

    @routes.post("/api/animation")
    async def animation_play(r):
        d = await body(r)
        if "spec" in d:
            engine.play(spec=d["spec"], brightness=d.get("brightness"))
        else:
            engine.play(d["name"], brightness=d.get("brightness"))
        return state()

    @routes.delete("/api/animation")
    async def animation_stop(_):
        engine.stop_animation()
        return state()

    @routes.put("/api/animations/{name}")
    async def animation_save(r):
        return web.json_response(engine.library.save("animations", r.match_info["name"], await body(r)))

    @routes.delete("/api/animations/{name}")
    async def animation_delete(r):
        engine.library.delete("animations", r.match_info["name"])
        return web.json_response({"deleted": r.match_info["name"]})

    # presets ---------------------------------------------------------------------------------
    @routes.get("/api/presets")
    async def presets_list(_):
        return web.json_response(engine.library.all("presets"))

    @routes.put("/api/presets/{name}")
    async def preset_save(r):
        return web.json_response(engine.library.save("presets", r.match_info["name"], await body(r)))

    @routes.delete("/api/presets/{name}")
    async def preset_delete(r):
        engine.library.delete("presets", r.match_info["name"])
        return web.json_response({"deleted": r.match_info["name"]})

    @routes.post("/api/presets/{name}/apply")
    async def preset_apply(r):
        engine.apply_preset(r.match_info["name"])
        return state()

    @routes.post("/api/presets/{name}/save-current")
    async def preset_from_current(r):
        s = engine.s
        spec = ({"animation": s["animation"]} if s["animation"] and s["animation"] != "(inline)"
                else {"effect": {"mode": s["effect"], "speed": s["speed"]}} if s["effect"]
                else {"color": s["rgb"]})
        spec["brightness"] = s["brightness"]
        return web.json_response(engine.library.save("presets", r.match_info["name"], spec))

    # raw / manual mode -----------------------------------------------------------------------
    @routes.get("/api/ops")
    async def ops(_):
        return web.json_response({k: {"params": v["params"], "doc": v["doc"]} for k, v in protocol.OPS.items()})

    @routes.post("/api/raw")
    async def raw(r):
        d = await body(r)
        if "hex" in d:
            plain = protocol.raw(d["hex"])
        else:
            plain = protocol.build(d["op"], **(d.get("args") or {}))
        engine.device.submit(plain, force=True)
        return web.json_response({"sent": plain.hex()})

    # live stream (screen / music clients) ----------------------------------------------------
    @routes.get("/api/stream")
    async def stream(r):
        ws = web.WebSocketResponse(heartbeat=20)
        await ws.prepare(r)
        async for msg in ws:
            if msg.type == WSMsgType.BINARY and len(msg.data) >= 3:  # r g b [brightness]
                b = msg.data
                engine.push_stream(b[0], b[1], b[2], b[3] if len(b) > 3 else 100)
            elif msg.type == WSMsgType.TEXT:
                try:
                    d = json.loads(msg.data)
                    rgb = d["rgb"]
                    engine.push_stream(rgb[0], rgb[1], rgb[2], d.get("brightness", 100))
                except (ValueError, KeyError, TypeError, IndexError):
                    await ws.send_json({"error": "expected {\"rgb\":[r,g,b],\"brightness\":0-100}"})
        return ws

    # state events ----------------------------------------------------------------------------
    @routes.get("/api/events")
    async def events(r):
        ws = web.WebSocketResponse(heartbeat=20)
        await ws.prepare(r)
        queue: asyncio.Queue = asyncio.Queue(maxsize=1)

        def on_change():
            if queue.full():
                queue.get_nowait()
            queue.put_nowait(engine.snapshot())

        engine.listeners.append(on_change)
        try:
            await ws.send_json(engine.snapshot())
            while not ws.closed:
                await ws.send_json(await queue.get())
        except (ConnectionResetError, asyncio.CancelledError):
            pass
        finally:
            engine.listeners.remove(on_change)
        return ws

    @routes.get("/healthz")
    async def health(_):
        return web.json_response({"ok": True})

    app = web.Application(middlewares=[guard])
    app.add_routes(routes)
    if os.path.isdir(WEB_DIR):
        app.router.add_get("/", lambda _: web.FileResponse(os.path.join(WEB_DIR, "index.html")))
        app.router.add_static("/static", WEB_DIR)
    return app
