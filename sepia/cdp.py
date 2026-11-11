"""Minimal Chrome DevTools Protocol client over --remote-debugging-pipe.

No websocket, no extension, no third-party driver: two file descriptors and
NUL-terminated JSON. This is the same channel Playwright uses, so it works with
branded Chrome builds that refuse --load-extension.
"""
from __future__ import annotations

import json
import os
import queue
import shutil
import subprocess
import threading
import time
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional

from . import util
from . import ws as ws_mod

LOG = util.get_logger("cdp")


class CDPError(RuntimeError):
    def __init__(self, message: str, code: Optional[int] = None):
        super().__init__(message)
        self.code = code


class CDP:
    def __init__(self, process: subprocess.Popen, websocket: ws_mod.WebSocket,
                 url: str, timeout: float = 20.0):
        self.process = process
        self.ws = websocket
        self.url = url
        self.timeout = timeout
        self._next_id = 0
        self._lock = threading.Lock()
        self._pending: Dict[int, "queue.Queue[Dict[str, Any]]"] = {}
        self._events: "queue.Queue[Dict[str, Any]]" = queue.Queue()
        self._handlers: List[Callable[[Dict[str, Any]], None]] = []
        self._closed = False
        self._reader = threading.Thread(target=self._read_loop, name="cdp-reader", daemon=True)
        self._reader.start()

    # -- plumbing ---------------------------------------------------------- #

    def _read_loop(self) -> None:
        while not self._closed:
            try:
                text = self.ws.recv_text(timeout=0.5)
            except ws_mod.WebSocketError:
                continue
            if text is None:
                break
            try:
                message = json.loads(text)
            except ValueError:
                LOG.debug("bad CDP frame: %r", text[:120])
                continue
            self._dispatch(message)
        self._closed = True
        for waiter in list(self._pending.values()):
            waiter.put({"error": {"message": "connection closed"}})
        self._pending.clear()
        self._events.put({"method": "__closed__"})

    def _dispatch(self, message: Dict[str, Any]) -> None:
        if "id" in message:
            waiter = self._pending.pop(message["id"], None)
            if waiter is not None:
                waiter.put(message)
            return
        if message.get("method", "").startswith("Target."):
            LOG.debug("event %s", message.get("method"))
        for handler in list(self._handlers):
            try:
                handler(message)
            except Exception as exc:  # noqa: BLE001 - a handler must not kill the reader
                LOG.debug("handler error: %s", exc)
        self._events.put(message)

    def send(self, method: str, params: Optional[Dict[str, Any]] = None,
             session_id: Optional[str] = None, timeout: Optional[float] = None) -> Dict[str, Any]:
        with self._lock:
            self._next_id += 1
            message_id = self._next_id
            payload: Dict[str, Any] = {"id": message_id, "method": method,
                                       "params": params or {}}
            if session_id:
                payload["sessionId"] = session_id
            waiter: "queue.Queue[Dict[str, Any]]" = queue.Queue(maxsize=1)
            self._pending[message_id] = waiter
            try:
                self.ws.send_text(json.dumps(payload, separators=(",", ":")))
            except ws_mod.WebSocketError as exc:
                self._pending.pop(message_id, None)
                raise CDPError(f"cannot write to the debug socket: {exc}") from exc
        try:
            response = waiter.get(timeout=timeout or self.timeout)
        except queue.Empty:
            self._pending.pop(message_id, None)
            raise CDPError(f"timeout waiting for {method}")
        if "error" in response:
            error = response["error"]
            raise CDPError(str(error.get("message", error)), error.get("code"))
        return response.get("result", {}) or {}

    def on_event(self, handler: Callable[[Dict[str, Any]], None]) -> None:
        self._handlers.append(handler)

    def wait_for(self, method: str, session_id: Optional[str] = None,
                 timeout: float = 30.0) -> Dict[str, Any]:
        deadline = time.time() + timeout
        while time.time() < deadline:
            remaining = max(0.05, deadline - time.time())
            try:
                message = self._events.get(timeout=remaining)
            except queue.Empty:
                break
            if message.get("method") == "__closed__":
                break
            if message.get("method") != method:
                continue
            if session_id and message.get("sessionId") != session_id:
                continue
            return message.get("params", {})
        raise CDPError(f"timed out waiting for event {method}")

    def close(self) -> None:
        self._closed = True
        try:
            self.send("Browser.close", timeout=2.0)
        except Exception:  # noqa: BLE001 - closing must never raise
            pass
        try:
            self.ws.close()
        except Exception:  # noqa: BLE001
            pass
        deadline = time.time() + 3
        while time.time() < deadline and self.process.poll() is None:
            time.sleep(0.05)
        if self.process.poll() is None:
            try:
                self.process.terminate()
            except Exception:  # noqa: BLE001
                pass


def launch(binary: str, profile_dir: Path, flags: List[str], env: Dict[str, str],
           timeout: float = 30.0) -> CDP:
    """Start Chrome with a local debugging port and connect over WebSocket.

    A pipe on fds 3/4 would be tidier, but Python's subprocess closes those fds
    after preexec_fn, and Chrome then refuses to start ("file descriptors are
    not open"). The port is bound to 127.0.0.1 and chosen by the kernel at random;
    the endpoint path carries a per-launch UUID.
    """
    cmd = [binary, "--remote-debugging-port=0", "--remote-debugging-address=127.0.0.1"] + flags
    debug_log = util.env("CDP_DEBUG")
    stderr_target: Any = subprocess.DEVNULL
    if debug_log:
        stderr_target = open(debug_log, "ab")
    process = subprocess.Popen(
        cmd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=stderr_target,
        start_new_session=True,
    )
    try:
        port, path = ws_mod.wait_for_devtools_port(profile_dir, timeout=timeout)
        url = f"ws://127.0.0.1:{port}{path}"
        websocket = ws_mod.WebSocket.connect(url, timeout=timeout)
    except Exception:
        try:
            process.terminate()
        except Exception:  # noqa: BLE001
            pass
        raise
    return CDP(process, websocket, url, timeout=timeout)


def prepare_target(cdp: CDP, session_id: str, *, timezone: Optional[str] = None,
                   locale: Optional[str] = None, hardware_concurrency: Optional[int] = None,
                   viewport: Optional[Dict[str, Any]] = None,
                   shim: Optional[str] = None) -> None:
    """Apply native overrides and install the shim before any page script runs."""
    try:
        cdp.send("Page.enable", session_id=session_id)
    except CDPError:
        pass
    if shim:
        try:
            cdp.send("Page.addScriptToEvaluateOnNewDocument",
                     {"source": shim, "runImmediately": True}, session_id=session_id)
        except CDPError as exc:
            LOG.debug("shim injection failed: %s", exc)
    if timezone:
        for method, params in (
            ("Emulation.setTimezoneOverride", {"timezoneId": timezone}),
            ("Emulation.setLocaleOverride", {"locale": locale or "en-US"}),
            ("Emulation.setHardwareConcurrencyOverride",
             {"hardwareConcurrency": hardware_concurrency}),
        ):
            if method.endswith("HardwareConcurrencyOverride") and not hardware_concurrency:
                continue
            try:
                cdp.send(method, params, session_id=session_id)
            except CDPError as exc:
                LOG.debug("%s failed: %s", method, exc)
    if viewport:
        try:
            cdp.send("Emulation.setDeviceMetricsOverride", viewport, session_id=session_id)
        except CDPError as exc:
            LOG.debug("device metrics failed: %s", exc)


def device_metrics(persona: Any, window: Optional[tuple] = None) -> Dict[str, Any]:
    width, height = window if window else (persona.screen_width - 60, persona.screen_height - 140)
    return {
        "width": width,
        "height": height,
        "screenWidth": persona.screen_width,
        "screenHeight": persona.screen_height,
        "screenOrientation": {"type": "landscapePrimary", "angle": 90},
        "deviceScaleFactor": persona.dpr,
        "mobile": False,
    }


def attach_all(cdp: CDP, setup: Callable[[str, Dict[str, Any]], None],
               timeout: float = 20.0) -> None:
    """Auto-attach to every existing and future page/iframe target."""
    cdp.send("Target.setAutoAttach",
             {"autoAttach": True, "waitForDebuggerOnStart": True, "flatten": True})

    def handler(message: Dict[str, Any]) -> None:
        if message.get("method") != "Target.attachedToTarget":
            return
        params = message.get("params") or {}
        session_id = params.get("sessionId")
        target_info = params.get("targetInfo") or {}
        if not session_id:
            return

        def work() -> None:
            # The reader thread must never block on send(): responses are read
            # by that very thread, so a nested request would deadlock.
            try:
                setup(str(session_id), target_info)
            except Exception as exc:  # noqa: BLE001
                LOG.debug("target setup failed: %s", exc)
            finally:
                try:
                    cdp.send("Runtime.runIfWaitingForDebugger",
                             session_id=str(session_id), timeout=10)
                except CDPError as exc:
                    LOG.debug("resume failed: %s", exc)

        threading.Thread(target=work, daemon=True).start()

    cdp.on_event(handler)
    try:
        cdp.send("Target.setDiscoverTargets", {"discover": True})
    except CDPError:
        pass
