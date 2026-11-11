"""Minimal RFC 6455 WebSocket client (text frames), stdlib only.

Used for the Chrome DevTools Protocol endpoint. Chrome speaks JSON text frames;
we need masking, fragmentation, ping/pong and a clean close — nothing else.
"""
from __future__ import annotations

import base64
import hashlib
import json
import os
import queue
import socket
import struct
import threading
import time
from typing import Any, Dict, Optional
from urllib.parse import urlsplit

from . import util

LOG = util.get_logger("ws")

GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

OP_CONT = 0x0
OP_TEXT = 0x1
OP_BINARY = 0x2
OP_CLOSE = 0x8
OP_PING = 0x9
OP_PONG = 0xA


class WebSocketError(RuntimeError):
    pass


class WebSocket:
    def __init__(self, sock: socket.socket, host: str, path: str, key: str):
        self.sock = sock
        self.host = host
        self.path = path
        self.key = key
        self._send_lock = threading.Lock()
        self._buffer = bytearray()
        self._closed = False
        self.messages: "queue.Queue[Optional[str]]" = queue.Queue()
        self._reader = threading.Thread(target=self._read_loop, name="ws-reader", daemon=True)
        self._reader.start()

    # -- handshake --------------------------------------------------------- #

    @classmethod
    def connect(cls, url: str, timeout: float = 10.0) -> "WebSocket":
        parts = urlsplit(url)
        if parts.scheme not in ("ws", "wss"):
            raise WebSocketError(f"unsupported websocket scheme {parts.scheme}")
        host = parts.hostname or "127.0.0.1"
        port = parts.port or (443 if parts.scheme == "wss" else 80)
        path = parts.path or "/"
        if parts.query:
            path += "?" + parts.query
        sock = socket.create_connection((host, port), timeout=timeout)
        sock.settimeout(None)
        if parts.scheme == "wss":
            import ssl

            context = ssl.create_default_context()
            sock = context.wrap_socket(sock, server_hostname=host)
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            # No Origin header: DevTools rejects cross-origin upgrades unless
            # --remote-allow-origins is passed, and we are a local client anyway.
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        sock.sendall(request.encode())
        head = bytearray()
        while b"\r\n\r\n" not in head:
            chunk = sock.recv(4096)
            if not chunk:
                raise WebSocketError("connection closed during handshake")
            head.extend(chunk)
            if len(head) > 65536:
                raise WebSocketError("handshake response too large")
        header, _, rest = bytes(head).partition(b"\r\n\r\n")
        status_line = header.split(b"\r\n", 1)[0].decode("iso-8859-1")
        if "101" not in status_line:
            raise WebSocketError(f"upgrade rejected: {status_line}")
        expected = base64.b64encode(
            hashlib.sha1((key + GUID).encode()).digest()).decode()
        if expected.encode() not in header:
            raise WebSocketError("bad Sec-WebSocket-Accept")
        ws = cls(sock, host, path, key)
        if rest:
            ws._buffer.extend(rest)
        return ws

    # -- framing ----------------------------------------------------------- #

    def send_text(self, text: str) -> None:
        self._send_frame(OP_TEXT, text.encode())

    def _send_frame(self, opcode: int, payload: bytes) -> None:
        header = bytearray([0x80 | opcode])
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        elif length < 65536:
            header.append(0x80 | 126)
            header.extend(struct.pack("!H", length))
        else:
            header.append(0x80 | 127)
            header.extend(struct.pack("!Q", length))
        mask = os.urandom(4)
        header.extend(mask)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        with self._send_lock:
            if self._closed:
                raise WebSocketError("socket closed")
            try:
                self.sock.sendall(bytes(header) + masked)
            except OSError as exc:
                self._closed = True
                raise WebSocketError(f"send failed: {exc}") from exc

    def _read_exact(self, count: int) -> bytes:
        while len(self._buffer) < count:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise WebSocketError("socket closed")
            self._buffer.extend(chunk)
        data = bytes(self._buffer[:count])
        del self._buffer[:count]
        return data

    def _read_loop(self) -> None:
        fragments: list = []
        try:
            while not self._closed:
                first = self._read_exact(2)
                fin = bool(first[0] & 0x80)
                opcode = first[0] & 0x0F
                masked = bool(first[1] & 0x80)
                length = first[1] & 0x7F
                if length == 126:
                    length = struct.unpack("!H", self._read_exact(2))[0]
                elif length == 127:
                    length = struct.unpack("!Q", self._read_exact(8))[0]
                mask = self._read_exact(4) if masked else None
                payload = self._read_exact(length) if length else b""
                if mask:
                    payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))

                if opcode == OP_PING:
                    self._send_frame(OP_PONG, payload)
                    continue
                if opcode == OP_PONG:
                    continue
                if opcode == OP_CLOSE:
                    try:
                        self._send_frame(OP_CLOSE, payload[:2])
                    except WebSocketError:
                        pass
                    break
                if opcode in (OP_TEXT, OP_BINARY):
                    fragments = [payload]
                    if fin:
                        self._emit(b"".join(fragments))
                    continue
                if opcode == OP_CONT:
                    fragments.append(payload)
                    if fin:
                        self._emit(b"".join(fragments))
                        fragments = []
        except (OSError, WebSocketError) as exc:
            LOG.debug("reader stopped: %s", exc)
        finally:
            self._closed = True
            self.messages.put(None)

    def _emit(self, payload: bytes) -> None:
        self.messages.put(payload.decode("utf-8", "replace"))

    def recv_text(self, timeout: Optional[float] = None) -> Optional[str]:
        try:
            return self.messages.get(timeout=timeout)
        except queue.Empty:
            raise WebSocketError("receive timeout")

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        try:
            sock = self.sock
            sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass
        self.messages.put(None)


def wait_for_devtools_port(profile_dir, timeout: float = 15.0) -> tuple:
    """Chrome writes '<port>\\n<path>' into DevToolsActivePort when ready."""
    from pathlib import Path

    target = Path(profile_dir) / "DevToolsActivePort"
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            lines = target.read_text().strip().splitlines()
        except OSError:
            lines = []
        if len(lines) >= 2:
            port_s, path = lines[0].strip(), lines[1].strip()
            if port_s.isdigit() and path.startswith("/devtools/"):
                return int(port_s), path
        time.sleep(0.05)
    raise WebSocketError("Chrome never published DevToolsActivePort")


def json_call(ws: WebSocket, payload: Dict[str, Any]) -> None:
    ws.send_text(json.dumps(payload, separators=(",", ":")))
