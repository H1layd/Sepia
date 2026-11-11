"""A small async HTTP/1.1 client that can travel through the proxy chain.

Used for health checks and for the leak-verification battery. Deliberately
minimal: no redirects, no cookies, no keep-alive, no cache. We want to see
exactly what the network sees.
"""
from __future__ import annotations

import asyncio
import ipaddress
import json
import ssl
from typing import Dict, List, Optional, Tuple
from urllib.parse import urljoin, urlsplit

from . import util

MAX_HEADER = 64 * 1024
MAX_BODY = 4 * 1024 * 1024


class HttpError(Exception):
    pass


class Response:
    def __init__(self, status: int, reason: str, headers: Dict[str, str], body: bytes):
        self.status = status
        self.reason = reason
        self.headers = headers
        self.body = body

    def json(self) -> dict:
        try:
            return json.loads(self.body.decode("utf-8", "replace"))
        except ValueError as exc:
            raise HttpError(f"response is not JSON: {self.body[:160]!r}") from exc

    def text(self) -> str:
        return self.body.decode("utf-8", "replace")


def default_headers(host: str) -> Dict[str, str]:
    return {
        "Host": host,
        "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 "
        "(KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
        "Accept": "text/html,application/json;q=0.9,*/*;q=0.8",
        "Accept-Language": "en-US,en;q=0.9",
        "Accept-Encoding": "identity",
        "Connection": "close",
    }


def tls_context() -> ssl.SSLContext:
    ctx = ssl.create_default_context()
    ctx.check_hostname = True
    ctx.verify_mode = ssl.CERT_REQUIRED
    try:
        ctx.set_alpn_protocols(["http/1.1"])
    except NotImplementedError:
        pass
    return ctx


async def open_tls(
    chain,
    host: str,
    port: int,
    server_hostname: Optional[str] = None,
    timeout: float = 15.0,
) -> Tuple[asyncio.StreamReader, asyncio.StreamWriter]:
    reader, writer = await chain.dial(host, port)
    ctx = tls_context()
    try:
        await _upgrade_to_tls(writer, ctx, server_hostname or host, timeout)
    except BaseException:
        writer.close()
        raise
    return reader, writer


async def _upgrade_to_tls(writer: asyncio.StreamWriter, ctx: ssl.SSLContext,
                          server_hostname: str, timeout: float) -> None:
    """Start TLS on an existing stream. StreamWriter.start_tls only exists on
    Python 3.11+, so 3.9/3.10 go through loop.start_tls and re-point the stream."""
    start_tls = getattr(writer, "start_tls", None)
    if start_tls is not None:
        await asyncio.wait_for(start_tls(ctx, server_hostname=server_hostname), timeout=timeout)
        return
    loop = asyncio.get_running_loop()
    transport = writer.transport
    protocol = transport.get_protocol()
    new_transport = await asyncio.wait_for(
        loop.start_tls(transport, protocol, ctx, server_hostname=server_hostname,
                       ssl_handshake_timeout=timeout),
        timeout=timeout + 1,
    )
    if new_transport is None:
        raise ConnectionError("TLS upgrade failed")
    writer._transport = new_transport  # type: ignore[attr-defined]
    if hasattr(protocol, "_transport"):
        protocol._transport = new_transport  # type: ignore[attr-defined]


async def _read_headers(reader: asyncio.StreamReader) -> Tuple[int, str, Dict[str, str]]:
    raw = await reader.readuntil(b"\r\n\r\n")
    if len(raw) > MAX_HEADER:
        raise HttpError("response headers too large")
    text = raw.decode("iso-8859-1")
    lines = text.split("\r\n")
    status_line = lines[0]
    parts = status_line.split(" ", 2)
    if len(parts) < 2 or not parts[0].startswith("HTTP/"):
        raise HttpError(f"bad status line: {status_line!r}")
    status = int(parts[1])
    reason = parts[2] if len(parts) > 2 else ""
    headers: Dict[str, str] = {}
    for line in lines[1:]:
        if not line:
            continue
        name, sep, value = line.partition(":")
        if not sep:
            continue
        headers[name.strip().lower()] = value.strip()
    return status, reason, headers


async def _read_body(
    reader: asyncio.StreamReader, status: int, headers: Dict[str, str]
) -> bytes:
    if status in (204, 304) or 100 <= status < 200:
        return b""
    if headers.get("transfer-encoding", "").lower() == "chunked":
        chunks: List[bytes] = []
        total = 0
        while True:
            line = await reader.readuntil(b"\r\n")
            size_s = line.split(b";")[0].strip()
            try:
                size = int(size_s, 16)
            except ValueError as exc:
                raise HttpError(f"bad chunk size {size_s!r}") from exc
            if size == 0:
                while True:
                    trailer = await reader.readuntil(b"\r\n")
                    if trailer in (b"\r\n", b""):
                        break
                break
            total += size
            if total > MAX_BODY:
                raise HttpError("response body too large")
            chunks.append(await reader.readexactly(size))
            await reader.readexactly(2)
        return b"".join(chunks)
    length_s = headers.get("content-length")
    if length_s:
        length = int(length_s)
        if length > MAX_BODY:
            raise HttpError("response body too large")
        return await reader.readexactly(length)
    # No framing: read until EOF, bounded.
    data = bytearray()
    while len(data) < MAX_BODY:
        chunk = await reader.read(16384)
        if not chunk:
            break
        data.extend(chunk)
    return bytes(data)


async def request(
    chain,
    url: str,
    method: str = "GET",
    headers: Optional[Dict[str, str]] = None,
    body: bytes = b"",
    timeout: float = 15.0,
    follow: int = 0,
) -> Response:
    resp = await _request_once(chain, url, method, headers, body, timeout)
    hops = 0
    while follow > 0 and _is_redirect(resp.status) and hops < follow:
        location = resp.headers.get("location")
        if not location:
            break
        url = urljoin(url, location)
        follow -= 1
        hops += 1
        resp = await _request_once(chain, url, "GET", headers, b"", timeout)
    return resp


def _is_redirect(status: int) -> bool:
    return status in (301, 302, 303, 307, 308)


async def _request_once(
    chain,
    url: str,
    method: str = "GET",
    headers: Optional[Dict[str, str]] = None,
    body: bytes = b"",
    timeout: float = 15.0,
) -> Response:
    parts = urlsplit(url)
    if parts.scheme not in ("http", "https"):
        raise HttpError(f"unsupported scheme {parts.scheme!r}")
    host = parts.hostname or ""
    if not host:
        raise HttpError(f"no host in {url!r}")
    port = parts.port or (443 if parts.scheme == "https" else 80)
    path = parts.path or "/"
    if parts.query:
        path = f"{path}?{parts.query}"

    hdrs = default_headers(host)
    if parts.port and parts.port not in (80, 443):
        hdrs["Host"] = f"{host}:{port}"
    if headers:
        hdrs.update(headers)

    async def run() -> Response:
        if parts.scheme == "https":
            reader, writer = await open_tls(chain, host, port, timeout=timeout)
        else:
            reader, writer = await chain.dial(host, port)
        try:
            request_lines = [f"{method} {path} HTTP/1.1"]
            if body:
                hdrs["Content-Length"] = str(len(body))
            request_lines.extend(f"{k}: {v}" for k, v in hdrs.items())
            writer.write(("\r\n".join(request_lines) + "\r\n\r\n").encode("utf-8") + body)
            await writer.drain()
            status, reason, resp_headers = await _read_headers(reader)
            payload = await _read_body(reader, status, resp_headers)
            return Response(status, reason, resp_headers, payload)
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except Exception:
                pass

    return await asyncio.wait_for(run(), timeout=timeout + 2)


async def get_json(chain, url: str, timeout: float = 15.0) -> dict:
    resp = await request(chain, url, timeout=timeout)
    if resp.status >= 400:
        raise HttpError(f"HTTP {resp.status} from {url}")
    return resp.json()


async def ip_echo(chain, url: str, timeout: float = 12.0) -> Dict[str, Optional[str]]:
    """Normalise the various 'what is my IP' endpoints into one shape."""
    resp = await request(chain, url, timeout=timeout, follow=3)
    if resp.status >= 400:
        raise HttpError(f"HTTP {resp.status} from {url}")
    text = resp.text()
    out: Dict[str, Optional[str]] = {"ip": None, "country": None, "dns": None, "raw": text[:400]}

    if resp.headers.get("content-type", "").startswith("application/json"):
        try:
            data = resp.json()
        except HttpError:
            data = {}
        for key in ("ip", "YourFuckingIPAddress", "query", "ip_addr"):
            if isinstance(data.get(key), str):
                out["ip"] = data[key]
                break
        if isinstance(data.get("country"), str):
            out["country"] = data["country"]
        elif isinstance(data.get("country_name"), str):
            out["country"] = data["country_name"]
        dns = data.get("dns")
        if isinstance(dns, dict) and isinstance(dns.get("ip"), str):
            out["dns"] = dns["ip"]
        elif isinstance(dns, str):
            out["dns"] = dns
    else:
        for line in text.splitlines():
            key, _, value = line.partition("=")
            key = key.strip()
            if key == "ip":
                out["ip"] = value.strip()
            elif key in ("loc", "country"):
                out["country"] = value.strip()
            elif key == "dns":
                out["dns"] = value.strip()
        if out["ip"] is None:
            candidate = text.strip()
            try:
                ipaddress.ip_address(candidate)
                out["ip"] = candidate
            except ValueError:
                pass
    return out
