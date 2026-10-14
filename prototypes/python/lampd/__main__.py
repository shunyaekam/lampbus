"""Run with:  python -m lampd

Environment (all optional):
  LAMPD_NAME      BLE name of the lamp               (SSL-E089ED)
  LAMPD_BIND      address for the HTTP API           (127.0.0.1; use `tailscale serve` to expose it)
  LAMPD_PORT      HTTP port                          (8787)
  LAMPD_IDLE      seconds before dropping BLE, 0=never (120)
  LAMPD_HOMEKIT   1/0 enable the HomeKit bridge      (1)
  LAMPD_HK_PORT   HomeKit bridge port                (51826)
  LAMPD_LIBRARY   directory with animations/ presets/ (./library)
  LAMPD_CONFIG    secrets/state directory            (~/.config/lampd)
  LAMPD_TOKEN     API token (default: generated into <config>/token)
"""
import asyncio
import logging
import os
import secrets
import signal
import stat

from aiohttp import web

from .api import make_app
from .device import Device
from .engine import Engine
from .library import Library

log = logging.getLogger("lampd")
HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def load_token(config_dir: str) -> str:
    if os.environ.get("LAMPD_TOKEN"):
        return os.environ["LAMPD_TOKEN"]
    path = os.path.join(config_dir, "token")
    if not os.path.exists(path):
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, stat.S_IRUSR | stat.S_IWUSR)
        with os.fdopen(fd, "w") as f:
            f.write(secrets.token_urlsafe(24))
        log.info("generated API token at %s", path)
    with open(path) as f:
        return f.read().strip()


async def amain():
    env = os.environ.get
    config_dir = os.path.expanduser(env("LAMPD_CONFIG", "~/.config/lampd"))
    os.makedirs(config_dir, mode=0o700, exist_ok=True)
    token = load_token(config_dir)

    device = Device(env("LAMPD_NAME", "SSL-E089ED"), idle_seconds=float(env("LAMPD_IDLE", "120")))
    device.start()
    engine = Engine(device, Library(os.path.expanduser(env("LAMPD_LIBRARY", os.path.normpath(os.path.join(HERE, "..", "..", "library"))))),
                    os.path.join(config_dir, "state.json"))

    runner = web.AppRunner(make_app(engine, token))
    await runner.setup()
    bind, port = env("LAMPD_BIND", "127.0.0.1"), int(env("LAMPD_PORT", "8787"))
    await web.TCPSite(runner, bind, port).start()
    log.info("API on http://%s:%d  (token: %s)", bind, port, os.path.join(config_dir, "token"))

    hk = None
    if env("LAMPD_HOMEKIT", "1") == "1":
        from .homekit import HomeKit
        hk = HomeKit(engine, asyncio.get_running_loop(), os.path.join(config_dir, "homekit.state"),
                     int(env("LAMPD_HK_PORT", "51826")))
        await hk.start()

    stop = asyncio.Event()
    for sig in (signal.SIGINT, signal.SIGTERM):
        asyncio.get_running_loop().add_signal_handler(sig, stop.set)
    await stop.wait()
    log.info("shutting down")
    if hk:
        await hk.stop()
    await runner.cleanup()
    await device.stop()


def main():
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(message)s")
    asyncio.run(amain())


if __name__ == "__main__":
    main()
