"""Local SOCKS5 + HTTP proxy with chain routing, and the control socket."""
from __future__ import annotations

import asyncio
import json
import os
import socket
import struct
import time
from typing import Dict, List, Optional, Tuple

from . import chain as chain_mod
from . import dnsd, socks5, util

LOG = util.get_logger("proxy")
MAX_HEAD = 32 * 1024
BUFSIZE = 65536


class Counters:
    def __init__(self) -> None:
        self.started = time.time()
        self.connections = 0
        self.active = 0
        self.blocked = 0
        self.direct = 0
        self.chained = 0
        self.failed = 0
        self.bytes_up = 0
        self.bytes_down = 0
        self.per_route: Dict[str, int] = {}
        self.errors: List[Dict[str, str]] = []

    def note_error(self, where: str, exc: BaseException) -> None:
        self.errors.append({"where": where, "error": f"{type(exc).__name__}: {exc}",
                            "at": time.strftime("%H:%M:%S")})
        del self.errors[:-20]

    def snapshot(self) -> Dict[str, object]:
        return {
            "uptime_s": int(time.time() - self.started),
            "connections": self.connections,
            "active": self.active,
            "blocked": self.blocked,
            "direct": self.direct,
            "chained": self.chained,
            "failed": self.failed,
            "bytes_up": self.bytes_up,
            "bytes_down": self.bytes_down,
            "bytes_up_h": util.human_bytes(self.bytes_up),
            "bytes_down_h": util.human_bytes(self.bytes_down),
            "per_route": dict(sorted(self.per_route.items(), key=lambda kv: -kv[1])[:20]),
            "errors": list(self.errors),
        }


def original_destination(sock: socket.socket) -> Optional[Tuple[str, int]]:
    """Recover the pre-NAT destination on Linux REDIRECT sockets. None elsewhere."""
    if util.platform() != "linux":
        return None
    family = sock.family
    try:
        if family == socket.AF_INET:
            raw = sock.getsockopt(socket.IPPROTO_IP, 80, 16)  # SO_ORIGINAL_DST
            if len(raw) < 8:
                return None
            port = struct.unpack("!H", raw[2:4])[0]
            addr = socket.inet_ntoa(raw[4:8])
            return addr, port
        if family == socket.AF_INET6:
            raw = sock.getsockopt(socket.IPPROTO_IPV6, 80, 28)  # IP6T_SO_ORIGINAL_DST
            if len(raw) < 24:
                return None
            port = struct.unpack("!H", raw[2:4])[0]
            addr = socket.inet_ntop(socket.AF_INET6, raw[8:24])
            return addr, port
    except OSError:
        return None
    return None


class ProxyServer:
    def __init__(self, cfg: Dict[str, object], chain: chain_mod.Chain,
                 router: chain_mod.Router, resolver: Optional[dnsd.Resolver],
                 transparent_ports: Optional[List[int]] = None):
        self.cfg = cfg
        self.chain = chain
        self.router = router
        self.resolver = resolver
        self.transparent_ports = set(transparent_ports or [])
        self.counters = Counters()
        self._servers: List[asyncio.AbstractServer] = []
        self._tasks: set = set()
        self._control: Optional[asyncio.AbstractServer] = None
        self.socks_addr: Optional[Tuple[str, int]] = None
        self.http_addr: Optional[Tuple[str, int]] = None
        self.redirect_addr: Optional[Tuple[str, int]] = None
        self.listen_cfg = cfg["listen"]
        run_path = util.Path(str(cfg.get("paths", {}).get("run", "~/.sepia/run")))
        self.state_path = run_path / f"state-{_instance_tag(cfg)}.json"

    # -- lifecycle --------------------------------------------------------- #

    async def start(self) -> None:
        socks_host, socks_port = util.split_hostport(str(self.listen_cfg["socks"]))
        http_host, http_port = util.split_hostport(str(self.listen_cfg["http"]))

        socks_srv = await asyncio.start_server(
            lambda r, w: self._guard(r, w, "socks"), socks_host, socks_port, backlog=512
        )
        self._servers.append(socks_srv)
        self.socks_addr = (socks_host, socks_port)

        http_srv = await asyncio.start_server(
            lambda r, w: self._guard(r, w, "http"), http_host, http_port, backlog=512
        )
        self._servers.append(http_srv)
        self.http_addr = (http_host, http_port)

        if self.transparent_ports:
            redir_host, redir_port = util.split_hostport(str(self.listen_cfg["redirect"]))
            redirect_srv = await asyncio.start_server(
                lambda r, w: self._guard(r, w, "redirect"), redir_host, redir_port, backlog=512
            )
            self._servers.append(redirect_srv)
            self.redirect_addr = (redir_host, redir_port)

        ctrl_host, ctrl_port = util.split_hostport(str(self.listen_cfg["control"]))
        self._control = await asyncio.start_server(
            self._handle_control, ctrl_host, ctrl_port, backlog=32
        )

        LOG.info("SOCKS5 on %s", util.join_hostport(*self.socks_addr))
        LOG.info("HTTP   on %s", util.join_hostport(*self.http_addr))
        LOG.info("control on %s", util.join_hostport(ctrl_host, ctrl_port))
        self._write_state()

    async def serve_forever(self) -> None:
        servers = list(self._servers)
        if self._control is not None:
            servers.append(self._control)
        await asyncio.gather(*(srv.serve_forever() for srv in servers))

    async def stop(self) -> None:
        # Let in-flight connections finish for a moment, then cancel the rest.
        if self._tasks:
            pending = list(self._tasks)
            try:
                await asyncio.wait(pending, timeout=1.0)
            except Exception:
                pass
            for task in list(self._tasks):
                if not task.done():
                    task.cancel()
            if self._tasks:
                await asyncio.gather(*list(self._tasks), return_exceptions=True)
        for srv in self._servers + ([self._control] if self._control else []):
            srv.close()
        for srv in self._servers + ([self._control] if self._control else []):
            try:
                await srv.wait_closed()
            except Exception:
                pass
        try:
            if self.state_path.exists():
                self.state_path.unlink()
        except OSError:
            pass

    def _write_state(self) -> None:
        payload = {
            "pid": os.getpid(),
            "started": int(time.time()),
            "socks": util.join_hostport(*self.socks_addr) if self.socks_addr else None,
            "http": util.join_hostport(*self.http_addr) if self.http_addr else None,
            "control": str(self.listen_cfg["control"]),
            "chain_depth": self.chain.depth,
        }
        try:
            util.write_json(self.state_path, payload)
        except OSError as exc:
            LOG.debug("could not write state: %s", exc)

    # -- connection entry -------------------------------------------------- #

    async def _guard(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
                     mode: str) -> None:
        task = asyncio.current_task()
        if task is not None:
            self._tasks.add(task)
        self.counters.connections += 1
        self.counters.active += 1
        try:
            if mode == "socks":
                await self._handle_socks(reader, writer)
            elif mode == "http":
                await self._handle_http(reader, writer, transparent=False)
            else:
                await self._handle_redirect(reader, writer)
        except (asyncio.IncompleteReadError, ConnectionResetError, BrokenPipeError):
            pass
        except Exception as exc:  # noqa: BLE001 - connection scoped
            self.counters.note_error(mode, exc)
            LOG.debug("%s handler error: %s", mode, exc)
        finally:
            self.counters.active -= 1
            if task is not None:
                self._tasks.discard(task)
            _close(writer)

    # -- upstream selection ------------------------------------------------ #

    async def _connect(self, host: str, port: int) -> Tuple[asyncio.StreamReader, asyncio.StreamWriter, str]:
        action = self.router.action_for(host)
        if action == "block":
            self.counters.blocked += 1
            raise PermissionError(f"blocked by rule: {host}")
        if action == "direct" or self.chain.depth == 0:
            reader, writer = await chain_mod.open_marked_connection(
                host, port, self.chain.connect_timeout)
            self.counters.direct += 1
            self._route_note(f"direct:{host}")
            return reader, writer, "direct"
        try:
            reader, writer = await self.chain.dial(host, port)
        except Exception:
            self.counters.failed += 1
            raise
        self.counters.chained += 1
        self._route_note(f"chain:{host}")
        return reader, writer, "chain"

    def _route_note(self, key: str) -> None:
        self.counters.per_route[key] = self.counters.per_route.get(key, 0) + 1

    # -- SOCKS5 ------------------------------------------------------------ #

    async def _handle_socks(self, reader: asyncio.StreamReader,
                            writer: asyncio.StreamWriter) -> None:
        try:
            host, port, command = await asyncio.wait_for(
                socks5.server_handshake(reader, writer), timeout=15
            )
        except socks5.Socks5Error as exc:
            LOG.debug("socks handshake failed: %s", exc)
            return
        if command != socks5.CMD_CONNECT:
            writer.write(socks5.build_reply(0x07))
            await writer.drain()
            return
        try:
            up_reader, up_writer, _route = await self._connect(host, port)
        except PermissionError:
            writer.write(socks5.build_reply(0x02))
            await writer.drain()
            return
        except Exception as exc:  # noqa: BLE001
            self.counters.note_error("socks-dial", exc)
            writer.write(socks5.build_reply(0x05))
            await writer.drain()
            return
        try:
            bound_host, bound_port = up_writer.get_extra_info("peername") or ("0.0.0.0", 0)
        except Exception:
            bound_host, bound_port = "0.0.0.0", 0
        writer.write(socks5.build_reply(0x00, str(bound_host), int(bound_port)))
        await writer.drain()
        await self._splice(reader, writer, up_reader, up_writer)

    # -- HTTP proxy -------------------------------------------------------- #

    async def _handle_http(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
                           transparent: bool = False) -> None:
        try:
            head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout=20)
        except (asyncio.IncompleteReadError, asyncio.LimitOverrunError):
            return
        if len(head) > MAX_HEAD:
            await _http_error(writer, 431, "request headers too large")
            return
        lines = head.decode("iso-8859-1").split("\r\n")
        try:
            method, target, version = lines[0].split(" ", 2)
        except ValueError:
            await _http_error(writer, 400, "bad request line")
            return
        headers = _parse_headers(lines[1:])

        if method.upper() == "CONNECT":
            host, host_port = util.split_hostport(target, 443)
            try:
                up_reader, up_writer, _ = await self._connect(host, host_port)
            except PermissionError:
                await _http_error(writer, 403, "destination blocked")
                return
            except Exception as exc:  # noqa: BLE001
                self.counters.note_error("connect-dial", exc)
                await _http_error(writer, 502, "upstream connection failed")
                return
            writer.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            await writer.drain()
            await self._splice(reader, writer, up_reader, up_writer)
            return

        if target.startswith("http://") or target.startswith("https://"):
            scheme, rest = target.split("://", 1)
            authority, _, path = rest.partition("/")
            path = "/" + path
            host, host_port = util.split_hostport(authority, 443 if scheme == "https" else 80)
            rewritten = f"{method} {path} {version}\r\n"
        else:
            host_header = headers.get("host")
            if not host_header:
                await _http_error(writer, 400, "no Host header")
                return
            host, host_port = util.split_hostport(host_header, 80)
            scheme = "http"
            if _host_header_has_explicit_port(headers.get("host", "")) and host_port == 80:
                pass
            rewritten = f"{method} {target} {version}\r\n"

        if scheme == "https" and not transparent:
            await _http_error(writer, 405, "https requires CONNECT")
            return

        body = b""
        if headers.get("content-length"):
            try:
                body = await asyncio.wait_for(reader.readexactly(int(headers["content-length"])),
                                              timeout=30)
            except (asyncio.IncompleteReadError, ValueError, asyncio.TimeoutError):
                await _http_error(writer, 400, "bad request body")
                return

        try:
            up_reader, up_writer, _ = await self._connect(host, host_port)
        except PermissionError:
            await _http_error(writer, 403, "destination blocked")
            return
        except Exception as exc:  # noqa: BLE001
            self.counters.note_error("http-dial", exc)
            await _http_error(writer, 502, "upstream connection failed")
            return

        try:
            forwarded = _rebuild_headers(headers, hop_by_hop=("proxy-connection", "proxy-authorization"))
            up_writer.write(rewritten.encode() + forwarded.encode() + b"\r\n" + body)
            await up_writer.drain()
        except Exception as exc:  # noqa: BLE001
            self.counters.note_error("http-write", exc)
            _close(up_writer)
            return
        await self._splice(reader, writer, up_reader, up_writer)

    # -- transparent / redirect -------------------------------------------- #

    async def _handle_redirect(self, reader: asyncio.StreamReader,
                               writer: asyncio.StreamWriter) -> None:
        sock = writer.get_extra_info("socket")
        dest = original_destination(sock) if sock is not None else None
        if dest is not None:
            # True transparent relay: destination recovered from the kernel.
            try:
                up_reader, up_writer, _ = await self._connect(*dest)
            except Exception as exc:  # noqa: BLE001
                self.counters.note_error("transparent-dial", exc)
                _close(writer)
                return
            await self._splice(reader, writer, up_reader, up_writer)
            return
        # No original destination information: fall back to HTTP/1.1 parsing,
        # which works for plaintext port 80 traffic only.
        await self._handle_http(reader, writer, transparent=True)

    # -- byte shuttling ---------------------------------------------------- #

    async def _splice(self, client_reader: asyncio.StreamReader,
                      client_writer: asyncio.StreamWriter,
                      up_reader: asyncio.StreamReader,
                      up_writer: asyncio.StreamWriter) -> None:
        up_task = asyncio.ensure_future(
            self._pump(client_reader, up_writer, "up"))
        down_task = asyncio.ensure_future(
            self._pump(up_reader, client_writer, "down"))
        try:
            await asyncio.gather(up_task, down_task)
        finally:
            for task in (up_task, down_task):
                task.cancel()
            _close(up_writer)
            _close(client_writer)

    async def _pump(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter,
                    direction: str) -> None:
        try:
            while True:
                chunk = await reader.read(BUFSIZE)
                if not chunk:
                    break
                if direction == "up":
                    self.counters.bytes_up += len(chunk)
                else:
                    self.counters.bytes_down += len(chunk)
                writer.write(chunk)
                await writer.drain()
        except (ConnectionResetError, BrokenPipeError, asyncio.IncompleteReadError):
            pass
        finally:
            try:
                if writer.can_write_eof():
                    writer.write_eof()
            except Exception:
                pass

    # -- control plane ---------------------------------------------------- #

    async def _handle_control(self, reader: asyncio.StreamReader,
                              writer: asyncio.StreamWriter) -> None:
        try:
            head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout=5)
        except (asyncio.IncompleteReadError, asyncio.LimitOverrunError, asyncio.TimeoutError):
            _close(writer)
            return
        try:
            request_line = head.split(b"\r\n", 1)[0].decode("iso-8859-1")
            method, path, _ = request_line.split(" ", 2)
        except ValueError:
            _close(writer)
            return
        if method.upper() != "GET":
            _close(writer)
            return
        path = path.split("?")[0]
        payload: Dict[str, object]
        status = "200 OK"
        if path in ("/status", "/"):
            payload = {
                "engine": "python",
                "pid": os.getpid(),
                "chain_depth": self.chain.depth,
                "hops": [h.describe() for h in self.chain.hops],
                "listen": {
                    "socks": util.join_hostport(*self.socks_addr) if self.socks_addr else None,
                    "http": util.join_hostport(*self.http_addr) if self.http_addr else None,
                },
                "transparent_ports": sorted(self.transparent_ports),
                "counters": self.counters.snapshot(),
                "dns": self.resolver.snapshot() if self.resolver else None,
                "rules": self.router.rules,
            }
        elif path == "/metrics":
            payload = self.counters.snapshot()
        elif path == "/health":
            payload = {"ok": True, "pid": os.getpid()}
        else:
            status = "404 Not Found"
            payload = {"error": "unknown endpoint"}
        body = json.dumps(payload, indent=2).encode()
        writer.write(
            f"HTTP/1.1 {status}\r\nContent-Type: application/json\r\n"
            f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode() + body
        )
        await writer.drain()
        _close(writer)


# --------------------------------------------------------------------------- #
# helpers
# --------------------------------------------------------------------------- #


def _instance_tag(cfg: Dict[str, object]) -> str:
    try:
        return str(util.split_hostport(str(cfg["listen"]["control"]))[1])  # type: ignore[index]
    except Exception:
        return "default"


def _parse_headers(lines: List[str]) -> Dict[str, str]:
    headers: Dict[str, str] = {}
    for line in lines:
        if not line:
            continue
        name, sep, value = line.partition(":")
        if not sep:
            continue
        headers[name.strip().lower()] = value.strip()
    return headers


def _rebuild_headers(headers: Dict[str, str], hop_by_hop: Tuple[str, ...]) -> str:
    out = []
    for name, value in headers.items():
        if name in hop_by_hop:
            continue
        out.append(f"{name}: {value}")
    out.append("Connection: close")
    return "\r\n".join(out) + "\r\n"


def _host_header_has_explicit_port(host_header: str) -> bool:
    if host_header.startswith("["):
        return "]:" in host_header
    return ":" in host_header


async def _http_error(writer: asyncio.StreamWriter, code: int, message: str) -> None:
    body = json.dumps({"error": message, "code": code}).encode()
    try:
        writer.write(
            f"HTTP/1.1 {code} Error\r\nContent-Type: application/json\r\n"
            f"Content-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode() + body
        )
        await writer.drain()
    except (ConnectionResetError, BrokenPipeError):
        pass


def _close(writer: asyncio.StreamWriter) -> None:
    try:
        writer.close()
    except Exception:
        pass
