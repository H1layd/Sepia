"""Minimal DNS wire-format handling (RFC 1035 + EDNS passthrough).

Only what a forwarding resolver needs: read the question, rewrite the header
flags, synthesise answers for blocked names, and scan TTLs for caching.
"""
from __future__ import annotations

import ipaddress
import struct
from typing import Dict, List, Optional, Tuple

TYPE_A = 1
TYPE_NS = 2
TYPE_CNAME = 5
TYPE_SOA = 6
TYPE_PTR = 12
TYPE_MX = 15
TYPE_AAAA = 28
TYPE_SRV = 33
TYPE_OPT = 41
TYPE_HTTPS = 65

TYPE_NAMES = {
    1: "A", 2: "NS", 5: "CNAME", 6: "SOA", 12: "PTR", 15: "MX", 16: "TXT",
    28: "AAAA", 33: "SRV", 41: "OPT", 43: "DS", 46: "RRSIG", 48: "DNSKEY",
    52: "TLSA", 64: "SVCB", 65: "HTTPS", 255: "ANY",
}

RCODE_NOERROR = 0
RCODE_FORMERR = 1
RCODE_SERVFAIL = 2
RCODE_NXDOMAIN = 3
RCODE_REFUSED = 5

FLAG_QR = 0x8000
FLAG_AA = 0x0400
FLAG_TC = 0x0200
FLAG_RD = 0x0100
FLAG_RA = 0x0080


class DnsError(Exception):
    pass


def _decode_label(raw: bytes) -> str:
    try:
        return raw.decode("ascii")
    except UnicodeDecodeError:
        pass
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        return raw.decode("latin-1")


def parse_name(data: bytes, offset: int) -> Tuple[str, int]:
    """Read a (possibly compressed) domain name. Returns (name, next_offset)."""
    labels: List[str] = []
    jumped = False
    next_offset = offset
    seen = 0
    while True:
        if offset >= len(data):
            raise DnsError("name runs past end of packet")
        length = data[offset]
        if length == 0:
            offset += 1
            if not jumped:
                next_offset = offset
            break
        if length & 0xC0 == 0xC0:
            if offset + 1 >= len(data):
                raise DnsError("truncated compression pointer")
            pointer = ((length & 0x3F) << 8) | data[offset + 1]
            if not jumped:
                next_offset = offset + 2
            jumped = True
            seen += 1
            if seen > 32:
                raise DnsError("compression loop")
            offset = pointer
            continue
        if length & 0xC0:
            raise DnsError(f"reserved label type 0x{length:02x}")
        offset += 1
        label = data[offset:offset + length]
        if len(label) != length:
            raise DnsError("truncated label")
        labels.append(_decode_label(label))
        offset += length
    return ".".join(labels), next_offset


def encode_name(name: str) -> bytes:
    name = name.strip(".")
    if not name:
        return b"\x00"
    out = bytearray()
    for label in name.split("."):
        raw = label.encode("idna")
        if not raw or len(raw) > 63:
            raise DnsError(f"bad label {label!r}")
        out.append(len(raw))
        out.extend(raw)
    out.append(0)
    return bytes(out)


class Question:
    __slots__ = ("name", "qtype", "qclass")

    def __init__(self, name: str, qtype: int, qclass: int):
        self.name = name
        self.qtype = qtype
        self.qclass = qclass

    def __repr__(self) -> str:  # pragma: no cover
        return f"<Q {self.name} {TYPE_NAMES.get(self.qtype, self.qtype)}>"


def parse_query(data: bytes) -> Dict[str, object]:
    if len(data) < 12:
        raise DnsError("short packet")
    ident, flags, qdcount, ancount, nscount, arcount = struct.unpack("!HHHHHH", data[:12])
    if qdcount < 1:
        raise DnsError("no question section")
    name, offset = parse_name(data, 12)
    if offset + 4 > len(data):
        raise DnsError("truncated question")
    qtype, qclass = struct.unpack("!HH", data[offset:offset + 4])
    offset += 4
    return {
        "id": ident,
        "flags": flags,
        "questions": int(qdcount),
        "name": name,
        "qtype": qtype,
        "qclass": qclass,
        "question_end": offset,
        "raw": data,
        # EDNS0 advertised UDP payload size, when present.
        "udp_size": _edns_udp_size(data, offset, int(arcount)),
    }


def _edns_udp_size(data: bytes, offset: int, arcount: int) -> int:
    if arcount <= 0:
        return 512
    try:
        for _ in range(arcount):
            _name, offset = parse_name(data, offset)
            rtype, _rclass, _ttl, rdlen = struct.unpack("!HHIH", data[offset:offset + 10])
            offset += 10
            if rtype == TYPE_OPT:
                return max(512, int(_rclass))
            offset += rdlen
    except DnsError:
        return 512
    return 512


def build_response(query: Dict[str, object], answers: List[Tuple[str, int, int, bytes]],
                   rcode: int = RCODE_NOERROR, authoritative: bool = False,
                   min_ttl: Optional[int] = None) -> bytes:
    """Build a reply carrying pre-encoded RDATA records (name, type, class, ttl, rdata)."""
    flags = FLAG_QR | FLAG_RA | (int(query["flags"]) & FLAG_RD)
    if authoritative:
        flags |= FLAG_AA
    flags = (flags & 0xFFF0) | (rcode & 0xF)
    out = bytearray(struct.pack("!HHHHHH", int(query["id"]), flags, 1,
                                len(answers), 0, 0))
    out.extend(encode_name(str(query["name"])))
    out.extend(struct.pack("!HH", int(query["qtype"]), int(query["qclass"])))
    for name, rtype, rclass, ttl, rdata in answers:
        ttl = max(0, min(ttl, min_ttl if min_ttl is not None else ttl))
        out.extend(encode_name(name))
        out.extend(struct.pack("!HHIH", rtype, rclass, ttl, len(rdata)))
        out.extend(rdata)
    return bytes(out)


def build_error(query: Dict[str, object], rcode: int) -> bytes:
    return build_response(query, [], rcode=rcode)


def build_address_response(query: Dict[str, object], address: str, ttl: int = 60) -> bytes:
    """Answer A/AAAA queries with a single fixed address (used for blocking)."""
    try:
        ip = ipaddress.ip_address(address)
    except ValueError:
        return build_error(query, RCODE_SERVFAIL)
    qtype = int(query["qtype"])
    qname = str(query["name"])
    if qtype == TYPE_A and ip.version != 4:
        return build_response(query, [])
    if qtype == TYPE_AAAA and ip.version != 6:
        return build_response(query, [])
    if qtype not in (TYPE_A, TYPE_AAAA):
        return build_response(query, [])
    return build_response(query, [(qname, qtype, 1, ttl, ip.packed)])


def scan_ttl(data: bytes) -> int:
    """Smallest TTL across all resource records; 0 if nothing usable."""
    try:
        _ident, _flags, qdcount, ancount, nscount, arcount = struct.unpack(
            "!HHHHHH", data[:12])
        offset = 12
        for _ in range(qdcount):
            _name, offset = parse_name(data, offset)
            offset += 4
        ttl: Optional[int] = None
        for _ in range(ancount + nscount + arcount):
            _name, offset = parse_name(data, offset)
            rtype, _rclass, rec_ttl, rdlen = struct.unpack("!HHIH", data[offset:offset + 10])
            offset += 10 + rdlen
            if rtype == TYPE_OPT:
                continue
            ttl = rec_ttl if ttl is None else min(ttl, rec_ttl)
        return int(ttl or 0)
    except (DnsError, struct.error):
        return 0


def rcode_of(data: bytes) -> int:
    if len(data) < 4:
        return RCODE_SERVFAIL
    return struct.unpack("!H", data[2:4])[0] & 0xF


def question_key(name: str, qtype: int, qclass: int = 1) -> str:
    return f"{name.lower().rstrip('.')}|{qtype}|{qclass}"
