"""Leak verification: prove what the network can and cannot see."""
from __future__ import annotations

import asyncio
import socket
from typing import Any, Dict, List, Optional, Tuple

from . import chain as chain_mod
from . import fingerprint, httpc, util
from .platforms.base import run

LOG = util.get_logger("verify")


class Verifier:
    def __init__(self, cfg: Dict[str, Any], chain: chain_mod.Chain,
                 run_dir=None):
        self.cfg = cfg
        self.chain = chain
        self.run_dir = run_dir
        self.timeout = float(cfg.get("verify", {}).get("timeout") or 12.0)

    # -- individual probes ------------------------------------------------- #

    async def exit_identity(self) -> Dict[str, Any]:
        endpoints = list(self.cfg.get("verify", {}).get("ip_endpoints") or [])
        results = []

        async def probe(url: str) -> Dict[str, Any]:
            try:
                info = await asyncio.wait_for(httpc.ip_echo(self.chain, url, self.timeout),
                                              timeout=self.timeout + 2)
                return {"endpoint": url, "ok": True, **info}
            except Exception as exc:  # noqa: BLE001 - surfaced in the report
                return {"endpoint": url, "ok": False, "error": f"{type(exc).__name__}: {exc}"}

        results = await asyncio.gather(*(probe(u) for u in endpoints))
        ips = sorted({r.get("ip") for r in results if r.get("ok") and r.get("ip")})
        countries = sorted({r.get("country") for r in results if r.get("ok") and r.get("country")})
        return {
            "exit_ips": ips,
            "consistent": len(ips) == 1,
            "countries": countries,
            "probes": results,
        }

    async def dns_position(self) -> Dict[str, Any]:
        url = str(self.cfg.get("verify", {}).get("dns_leak_endpoint") or "")
        if not url:
            return {"ok": False, "error": "no dns_leak_endpoint configured"}
        try:
            info = await httpc.ip_echo(self.chain, url, self.timeout)
            return {"ok": True, "resolver_seen": info.get("dns"), "exit_ip": info.get("ip"),
                    "raw": info.get("raw")}
        except Exception as exc:  # noqa: BLE001
            return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}

    async def http_headers(self) -> Dict[str, Any]:
        url = str(self.cfg.get("verify", {}).get("header_endpoint") or "")
        if not url:
            return {"ok": False, "error": "no header_endpoint configured"}
        try:
            resp = await httpc.request(self.chain, url, timeout=self.timeout,
                                       headers={"Accept": "application/json"})
            if resp.status >= 400:
                return {"ok": False, "status": resp.status,
                        "headers": dict(resp.headers)}
            data = resp.json()
            return {"ok": True, "headers": data.get("headers", {}),
                    "origin": data.get("origin")}
        except Exception as exc:  # noqa: BLE001
            return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}

    async def ipv6_direct(self) -> Dict[str, Any]:
        """If IPv6 egress still works, a dual-stack leak is possible."""
        try:
            reader, writer = await asyncio.wait_for(
                asyncio.open_connection("2606:4700:4700::1111", 443), timeout=6
            )
            writer.close()
            return {"ok": True, "leak": True,
                    "note": "IPv6 egress reachable without the chain"}
        except (OSError, asyncio.TimeoutError) as exc:
            return {"ok": True, "leak": False, "note": f"IPv6 blocked ({type(exc).__name__})"}

    async def enforcement(self) -> Dict[str, Any]:
        """Prove enforcement, not just that something failed.

        A transparent redirect makes "curl without a proxy" succeed — that is
        correct behaviour, not a leak, as long as the traffic leaves through the
        chain. So compare exit identities, and probe a port that is neither
        redirected nor allowed: if that port answers, the killswitch is open.
        """
        proxy = str(self.cfg["listen"]["http"])
        trace = "https://1.1.1.1/cdn-cgi/trace"

        direct, through = await asyncio.gather(
            _curl_identity(trace, proxy=None), _curl_identity(trace, proxy=proxy)
        )
        open_port = await _tcp_probe("8.8.8.8", 853, timeout=4.0)

        dns_before = self._dns_query_count()
        external_dns = await _dns_probe("example.com", "8.8.8.8", 53)
        dns_after = self._dns_query_count()
        intercepted = (
            dns_before is not None and dns_after is not None and dns_after > dns_before
        )
        loopback_dns = util.split_hostport(str(self.cfg["listen"]["dns"]))
        local_dns = await _dns_probe("example.com", loopback_dns[0], loopback_dns[1])

        direct_ok = bool(direct.get("ok"))
        via_proxy = bool(through.get("ok"))
        same_exit = bool(direct.get("ip") and through.get("ip")
                         and direct["ip"] == through["ip"])
        # direct works, leaves by a different address than the chain -> leak
        leak = direct_ok and via_proxy and not same_exit
        # a non-redirected port that still answers -> nothing is denying egress
        port_open = bool(open_port.get("ok"))
        dns_leak = bool(external_dns.get("ok")) and not intercepted

        return {
            "direct_http": direct,
            "through_proxy": through,
            "same_exit": same_exit,
            "open_port": open_port,
            "external_dns": external_dns,
            "dns_intercepted": intercepted,
            "local_dns": local_dns,
            "leak": leak,
            "dns_leak": dns_leak,
            "killswitch_effective": (not leak) and (not port_open),
        }

    def _dns_query_count(self) -> Optional[int]:
        from . import cli as cli_mod

        payload = cli_mod.control_get(self.cfg, "/status", timeout=2.0)
        if not payload:
            return None
        dns_info = payload.get("dns") or {}
        try:
            return int(dns_info.get("queries"))
        except (TypeError, ValueError):
            return None

    # -- aggregate --------------------------------------------------------- #

    async def run(self) -> Dict[str, Any]:
        exit_id, dns_pos, headers, ipv6 = await asyncio.gather(
            self.exit_identity(), self.dns_position(), self.http_headers(), self.ipv6_direct()
        )
        enforcement = await self.enforcement()
        audit = {
            "dns_config": fingerprint.configured_dns(),
            "ipv6_local": fingerprint.ipv6_exposure(),
            "hostname": socket.gethostname(),
        }
        results = {
            "chain": {
                "depth": self.chain.depth,
                "hops": [h.describe() for h in self.chain.hops],
            },
            "exit": exit_id,
            "dns": dns_pos,
            "headers": headers,
            "ipv6": ipv6,
            "enforcement": enforcement,
            "host": audit,
            "tls": fingerprint.egress_tls_report(),
            "findings": [],
        }
        results["findings"] = findings(results)
        return results


def findings(results: Dict[str, Any]) -> List[Dict[str, str]]:
    out: List[Dict[str, str]] = []

    def add(level: str, title: str, detail: str) -> None:
        out.append({"level": level, "title": title, "detail": detail})

    exit_info = results.get("exit") or {}
    if not exit_info.get("exit_ips"):
        add("fail", "no exit identity", "every IP-echo endpoint failed; chain is probably down")
    elif not exit_info.get("consistent"):
        add("warn", "inconsistent exit IPs",
            f"endpoints disagree: {', '.join(exit_info.get('exit_ips') or [])}")

    dns_pos = results.get("dns") or {}
    if dns_pos.get("ok") and dns_pos.get("resolver_seen"):
        add("info", "DNS resolver as seen from outside",
            f"upstream resolver {dns_pos['resolver_seen']}")
    elif dns_pos.get("ok") is False:
        add("warn", "DNS leak probe unavailable", str(dns_pos.get("error")))

    enf = results.get("enforcement") or {}
    if enf.get("killswitch_effective"):
        add("pass", "killswitch holds",
            "non-redirected ports are denied; nothing escapes around the chain")
    else:
        detail = ("a non-redirected port answered" if (enf.get("open_port") or {}).get("ok")
                  else "direct traffic left by a different address than the chain")
        add("fail", "killswitch not enforced", detail)
    if (enf.get("through_proxy") or {}).get("ok"):
        add("pass", "proxy path works",
            f"exit {enf['through_proxy'].get('ip')} through the local proxy")
    else:
        add("fail", "proxy path broken", str((enf.get("through_proxy") or {}).get("error")))
    if enf.get("same_exit"):
        add("pass", "transparent path and proxy share one exit",
            f"both leave as {enf['direct_http'].get('ip')}")
    elif (enf.get("direct_http") or {}).get("ok"):
        add("warn", "transparent path exits elsewhere",
            f"{enf['direct_http'].get('ip')} vs {(enf.get('through_proxy') or {}).get('ip')}")
    if enf.get("dns_leak"):
        add("fail", "plaintext DNS escapes",
            f"8.8.8.8 answered and sepia's resolver was not involved")
    elif enf.get("dns_intercepted"):
        add("pass", "plaintext DNS is intercepted",
            "queries to 8.8.8.8 were answered by sepia's DoH resolver")
    elif (enf.get("external_dns") or {}).get("ok"):
        add("warn", "plaintext DNS answered, interception unproven",
            "run this with the daemon up so the resolver counters are readable")
    else:
        add("pass", "plaintext DNS blocked", "external resolver did not answer")
    if not (enf.get("local_dns") or {}).get("ok"):
        add("warn", "local resolver did not answer",
            str((enf.get("local_dns") or {}).get("error")))

    ipv6 = results.get("ipv6") or {}
    if ipv6.get("leak"):
        add("fail", "IPv6 egress open", "dual-stack traffic can escape the chain")
    elif ipv6.get("ok"):
        add("pass", "IPv6 blocked", str(ipv6.get("note")))

    dns_cfg = (results.get("host") or {}).get("dns_config") or {}
    if dns_cfg.get("servers") and not dns_cfg.get("loopback_only"):
        add("warn", "system resolver not pointed at sepia",
            f"configured: {', '.join(dns_cfg['servers'])}")

    hostname = (results.get("host") or {}).get("hostname")
    if hostname and hostname.lower() not in ("localhost", ""):
        add("info", "hostname visible on the LAN", hostname)

    tls = (results.get("tls") or {}).get("default") or {}
    if tls.get("ja3_md5"):
        add("info", "TLS fingerprint (JA3)", f"{tls['ja3_md5']} — {tls.get('cipher_count')} ciphers, "
                                           f"{tls.get('extension_count')} extensions")

    if not any(f["level"] == "fail" for f in out):
        add("pass", "no hard leaks found", "review the informational findings anyway")
    return out


# --------------------------------------------------------------------------- #
# subprocess probes
# --------------------------------------------------------------------------- #


async def _curl(url: str, proxy: Optional[str], timeout: int = 10) -> Dict[str, Any]:
    """Run curl and report status plus the body (used for identity probes)."""
    cmd = ["curl", "-sS", "-m", str(timeout), "-w", "\n%{http_code}"]
    if proxy is None:
        cmd += ["--noproxy", "*"]
    else:
        cmd += ["-x", f"http://{proxy}"]
    cmd.append(url)
    try:
        rc, out, err = await asyncio.to_thread(run, cmd, False)
    except Exception as exc:  # noqa: BLE001
        return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
    body, _, code = out.rpartition("\n")
    ok = rc == 0 and code.strip().isdigit() and int(code) < 500
    return {"ok": ok, "code": code.strip() or None, "rc": rc, "body": body,
            "error": None if ok else util.redact_hostport((err or out).strip()[:200])}


async def _curl_identity(url: str, proxy: Optional[str], timeout: int = 10
                         ) -> Dict[str, Any]:
    result = await _curl(url, proxy, timeout)
    identity: Optional[str] = None
    for line in str(result.get("body") or "").splitlines():
        if line.startswith("ip="):
            identity = line.split("=", 1)[1].strip()
            break
    result["ip"] = identity
    return result


async def _tcp_probe(host: str, port: int, timeout: float = 4.0) -> Dict[str, Any]:
    """Does a raw TCP connection to this host:port succeed?"""
    def _try() -> None:
        with socket.create_connection((host, port), timeout=timeout):
            pass

    try:
        await asyncio.wait_for(asyncio.to_thread(_try), timeout=timeout + 1)
        return {"ok": True}
    except Exception as exc:  # noqa: BLE001
        return {"ok": False, "error": f"{type(exc).__name__}"}


async def _dns_probe(name: str, server: str, port: int = 53,
                     timeout: float = 5.0) -> Dict[str, Any]:
    """UDP DNS query without external tools, so this works on Windows too."""
    import struct as _struct

    from . import dnsmsg

    query = (_struct.pack("!HHHHHH", util.int_id(), 0x0100, 1, 0, 0, 0)
             + dnsmsg.encode_name(name) + _struct.pack("!HH", dnsmsg.TYPE_A, 1))

    def _ask() -> bytes:
        family, _t, _p, _c, sockaddr = socket.getaddrinfo(
            server, port, type=socket.SOCK_DGRAM)[0]
        sock = socket.socket(family, socket.SOCK_DGRAM)
        sock.settimeout(timeout)
        try:
            sock.sendto(query, sockaddr)
            data, _peer = sock.recvfrom(4096)
            return data
        finally:
            sock.close()

    try:
        data = await asyncio.wait_for(asyncio.to_thread(_ask), timeout=timeout + 1)
    except (asyncio.TimeoutError, OSError) as exc:
        return {"ok": False, "error": f"{type(exc).__name__}: no answer"}
    rcode = dnsmsg.rcode_of(data)
    answers = _answers(data)
    return {"ok": rcode == 0 and bool(answers), "rcode": rcode, "answers": answers[:5],
            "error": None if rcode == 0 else f"rcode {rcode}"}


def _answers(data: bytes) -> List[str]:
    import struct as _struct

    from . import dnsmsg

    try:
        _ident, _flags, qd, an, _ns, _ar = _struct.unpack("!HHHHHH", data[:12])
        offset = 12
        for _ in range(qd):
            _name, offset = dnsmsg.parse_name(data, offset)
            offset += 4
        out: List[str] = []
        for _ in range(an):
            name, offset = dnsmsg.parse_name(data, offset)
            rtype, _rclass, _ttl, rdlen = _struct.unpack("!HHIH", data[offset:offset + 10])
            offset += 10
            rdata = data[offset:offset + rdlen]
            offset += rdlen
            if rtype == dnsmsg.TYPE_A and rdlen == 4:
                out.append(socket.inet_ntoa(rdata))
            else:
                out.append(f"{name} type{rtype}")
        return out
    except Exception:  # noqa: BLE001 - display only
        return []


def render(results: Dict[str, Any]) -> str:
    lines: List[str] = []
    chain_info = results.get("chain") or {}
    lines.append(f"chain      : {chain_info.get('depth')} hop(s)")
    for hop in chain_info.get("hops") or []:
        lines.append(f"             - {hop}")
    exit_info = results.get("exit") or {}
    lines.append(f"exit ip    : {', '.join(exit_info.get('exit_ips') or ['?'])}"
                 f"{'  (' + ', '.join(exit_info['countries']) + ')' if exit_info.get('countries') else ''}")
    for probe in exit_info.get("probes") or []:
        status = "ok" if probe.get("ok") else "FAIL"
        detail = probe.get("ip") or probe.get("error")
        lines.append(f"             {status:<4} {probe.get('endpoint')} -> {detail}")

    dns_pos = results.get("dns") or {}
    if dns_pos.get("ok"):
        lines.append(f"dns   seen : resolver {dns_pos.get('resolver_seen')}")
    else:
        lines.append(f"dns   seen : unavailable ({dns_pos.get('error')})")

    headers = (results.get("headers") or {}).get("headers") or {}
    lines.append(f"user-agent : {headers.get('User-Agent', '?')[:70]}")
    tls = (results.get("tls") or {}).get("default") or {}
    if tls.get("ja3_md5"):
        lines.append(f"ja3        : {tls['ja3_md5']} ({tls.get('size_bytes')}B hello)")

    lines.append("")
    lines.append("findings:")
    icons = {"pass": "OK  ", "warn": "WARN", "fail": "FAIL", "info": "info"}
    for finding in results.get("findings") or []:
        icon = icons.get(finding["level"], finding["level"])
        lines.append(f"  [{icon}] {finding['title']}")
        lines.append(f"         {finding['detail']}")
    return "\n".join(lines)
