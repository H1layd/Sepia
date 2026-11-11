"""SOCKS5 protocol: client side (to reach hops) and server side (for local clients)."""
from __future__ import annotations

import asyncio
import ipaddress
import struct
from typing import Optional, Tuple

from . import util

REPLIES = {
    0x00: "succeeded",
    0x01: "general failure",
    0x02: "connection not allowed by ruleset",
    0x03: "network unreachable",
    0x04: "host unreachable",
    0x05: "connection refused",
    0x06: "TTL expired",
    0x07: "command not supported",
    0x08: "address type not supported",
}

CMD_CONNECT = 0x01
CMD_BIND = 0x02
CMD_UDP = 0x03

ATYP_V4 = 0x01
ATYP_DOMAIN = 0x03
ATYP_V6 = 0x04


class Socks5Error(Exception):
    def __init__(self, code: int, message: Optional[str] = None):
        self.code = code
        self.message = message or REPLIES.get(code, f"error {code}")
        super().__init__(self.message)


def encode_address(host: str, port: int) -> bytes:
    """ATYP+ADDR+PORT as used in client requests and server replies."""
    try:
        ip = ipaddress.ip_address(host)
    except ValueError:
        raw = host.encode("idna")
        if len(raw) > 255:
            raise Socks5Error(0x08, "hostname too long")
        return bytes([ATYP_DOMAIN, len(raw)]) + raw + struct.pack("!H", port)
    if ip.version == 4:
        return bytes([ATYP_V4]) + ip.packed + struct.pack("!H", port)
    return bytes([ATYP_V6]) + ip.packed + struct.pack("!H", port)


async def read_address(reader: asyncio.StreamReader) -> Tuple[str, int]:
    atyp = (await reader.readexactly(1))[0]
    if atyp == ATYP_V4:
        host = str(ipaddress.IPv4Address(await reader.readexactly(4)))
    elif atyp == ATYP_V6:
        host = str(ipaddress.IPv6Address(await reader.readexactly(16)))
    elif atyp == ATYP_DOMAIN:
        length = (await reader.readexactly(1))[0]
        host = util.decode_hostname(await reader.readexactly(length))
    else:
        raise Socks5Error(0x08, f"bad address type {atyp}")
    port = struct.unpack("!H", await reader.readexactly(2))[0]
    return host, port


def build_reply(code: int, host: str = "0.0.0.0", port: int = 0) -> bytes:
    try:
        body = encode_address(host, port)
    except Socks5Error:
        body = encode_address("0.0.0.0", 0)
    return bytes([0x05, code]) + b"\x00" + body


# --------------------------------------------------------------------------- #
# client side
# --------------------------------------------------------------------------- #


async def client_handshake(
    reader: asyncio.StreamReader,
    writer: asyncio.StreamWriter,
    host: str,
    port: int,
    username: Optional[str] = None,
    password: Optional[str] = None,
) -> None:
    """Negotiate auth and issue CONNECT on an already-open hop connection."""
    methods = [0x00]
    if username is not None:
        methods = [0x02]
    writer.write(bytes([0x05, len(methods)]) + bytes(methods))
    await writer.drain()

    version, method = await reader.readexactly(2)
    if version != 0x05:
        raise Socks5Error(0x01, "hop did not speak SOCKS5")
    if method == 0x02:
        user = (username or "").encode()
        pwd = (password or "").encode()
        if len(user) > 255 or len(pwd) > 255:
            raise Socks5Error(0x01, "credentials too long")
        writer.write(b"\x01" + bytes([len(user)]) + user + bytes([len(pwd)]) + pwd)
        await writer.drain()
        auth_version, status = await reader.readexactly(2)
        if auth_version != 0x01 or status != 0x00:
            raise Socks5Error(0x02, "hop rejected credentials")
    elif method != 0x00:
        raise Socks5Error(0x02, "hop accepted no offered auth method")

    writer.write(bytes([0x05, CMD_CONNECT, 0x00]) + encode_address(host, port))
    await writer.drain()

    version, reply, _reserved = await reader.readexactly(3)
    if version != 0x05:
        raise Socks5Error(0x01, "bad SOCKS5 reply version")
    await read_address(reader)  # bound address, unused for CONNECT
    if reply != 0x00:
        raise Socks5Error(reply)


# --------------------------------------------------------------------------- #
# server side
# --------------------------------------------------------------------------- #


async def server_handshake(
    reader: asyncio.StreamReader,
    writer: asyncio.StreamWriter,
    username: Optional[str] = None,
    password: Optional[str] = None,
) -> Tuple[str, int, int]:
    """Returns (host, port, command). Raises Socks5Error on protocol failure."""
    version, nmethods = await reader.readexactly(2)
    if version != 0x05:
        raise Socks5Error(0x01, "not SOCKS5")
    methods = await reader.readexactly(nmethods)
    need_auth = username is not None
    chosen = 0x02 if need_auth else 0x00
    if chosen not in methods:
        writer.write(bytes([0x05, 0xFF]))
        await writer.drain()
        raise Socks5Error(0x02, "no acceptable auth method")
    writer.write(bytes([0x05, chosen]))
    await writer.drain()

    if need_auth:
        auth_version = (await reader.readexactly(1))[0]
        if auth_version != 0x01:
            raise Socks5Error(0x01, "bad auth version")
        user_len = (await reader.readexactly(1))[0]
        user = (await reader.readexactly(user_len)).decode("utf-8", "replace")
        pass_len = (await reader.readexactly(1))[0]
        pwd = (await reader.readexactly(pass_len)).decode("utf-8", "replace")
        ok = user == username and pwd == (password or "")
        writer.write(b"\x01" + (b"\x00" if ok else b"\x01"))
        await writer.drain()
        if not ok:
            raise Socks5Error(0x02, "auth failed")

    version, command, _reserved = await reader.readexactly(3)
    if version != 0x05:
        raise Socks5Error(0x01, "bad request version")
    host, port = await read_address(reader)
    return host, port, command
