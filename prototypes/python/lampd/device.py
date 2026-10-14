"""Owns the one BLE connection to the lamp.

Callers submit plaintext frames; the newest pending frame wins (older ones are dropped, which is
what you want for animations and screen/music streams), identical consecutive frames are skipped,
writes are paced, and the connection is opened on demand and dropped after an idle period so the
phone app can still connect when you want it.
"""
from __future__ import annotations

import asyncio
import logging
from typing import Callable, Optional

from bleak import BleakClient, BleakScanner

from . import protocol

log = logging.getLogger("lampd.device")


class Device:
    def __init__(self, name: str, idle_seconds: float = 120.0, min_interval: float = 0.025):
        self.name = name
        self.idle_seconds = idle_seconds
        self.min_interval = min_interval
        self.on_connection_change: Optional[Callable[[bool], None]] = None
        self._client: Optional[BleakClient] = None
        self._ble_device = None
        self._pending: Optional[bytes] = None
        self._last_sent: Optional[bytes] = None
        self._wake = asyncio.Event()
        self._holds = 0
        self._idle_handle = None
        self._task: Optional[asyncio.Task] = None
        self.last_error: Optional[str] = None

    # -- public -----------------------------------------------------------------------------
    @property
    def connected(self) -> bool:
        return bool(self._client and self._client.is_connected)

    def start(self):
        self._task = asyncio.ensure_future(self._run())

    async def stop(self):
        if self._task:
            self._task.cancel()
        await self._disconnect()

    def submit(self, plain: bytes, force: bool = False):
        """Queue a frame (call from the event loop). Newest wins."""
        if force:
            self._last_sent = None
        self._pending = plain
        self._wake.set()

    def hold(self, delta: int):
        """Keep the connection open while >0 (animations, streams)."""
        self._holds = max(0, self._holds + delta)
        if self._holds == 0:
            self._arm_idle()
        elif self._idle_handle:
            self._idle_handle.cancel()
            self._idle_handle = None

    # -- internals --------------------------------------------------------------------------
    async def _connect(self):
        if self.connected:
            return
        for attempt in (1, 2):
            try:
                if self._ble_device is None or attempt == 2:
                    log.info("scanning for %s", self.name)
                    self._ble_device = await BleakScanner.find_device_by_filter(
                        lambda d, a: (a.local_name or "") == self.name or d.address == self.name, timeout=15)
                    if not self._ble_device:
                        raise RuntimeError(f"{self.name} not found (is another app connected to it?)")
                client = BleakClient(self._ble_device, timeout=20, disconnected_callback=self._on_disconnect)
                await client.connect()
                self._client = client
                self._last_sent = None  # lamp state unknown after a reconnect
                log.info("connected to %s", self.name)
                self._notify_connection(True)
                return
            except Exception as e:  # noqa: BLE001
                self.last_error = str(e)
                log.warning("connect attempt %d failed: %s", attempt, e)
                self._ble_device = None
        raise RuntimeError(self.last_error or "connect failed")

    def _on_disconnect(self, _client):
        log.info("disconnected")
        self._client = None
        self._notify_connection(False)

    def _notify_connection(self, up: bool):
        if self.on_connection_change:
            try:
                self.on_connection_change(up)
            except Exception:  # noqa: BLE001
                log.exception("connection listener failed")

    async def _disconnect(self):
        client, self._client = self._client, None
        if client:
            try:
                await client.disconnect()
            except Exception:  # noqa: BLE001
                pass

    def _arm_idle(self):
        if self._idle_handle:
            self._idle_handle.cancel()
            self._idle_handle = None
        if self.idle_seconds > 0 and self._holds == 0 and self.connected:
            loop = asyncio.get_event_loop()
            self._idle_handle = loop.call_later(self.idle_seconds, lambda: asyncio.ensure_future(self._idle_drop()))

    async def _idle_drop(self):
        if self._holds == 0 and self._pending is None:
            log.info("idle for %ss, disconnecting", self.idle_seconds)
            await self._disconnect()
            self._notify_connection(False)

    async def _run(self):
        backoff = 1.0
        while True:
            await self._wake.wait()
            self._wake.clear()
            plain, self._pending = self._pending, None
            if plain is None or plain == self._last_sent:
                continue
            try:
                await self._connect()
                await self._client.write_gatt_char(protocol.WRITE_CHAR, protocol.encrypt(plain), response=False)
                self._last_sent = plain
                self.last_error = None
                backoff = 1.0
                self._arm_idle()
            except asyncio.CancelledError:
                raise
            except Exception as e:  # noqa: BLE001
                self.last_error = str(e)
                log.warning("write failed: %s (retry in %.0fs)", e, backoff)
                await self._disconnect()
                if self._pending is None:
                    self._pending = plain
                await asyncio.sleep(backoff)
                backoff = min(backoff * 2, 15.0)
                self._wake.set()
                continue
            await asyncio.sleep(self.min_interval)
