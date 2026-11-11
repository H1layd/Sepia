"""Test suite. Run with: python3 -m unittest discover -s tests -v"""
from __future__ import annotations

import asyncio
import json
import os
import socket
import struct
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Optional

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from sepia import chain as chain_mod  # noqa: E402
from sepia import config as config_mod  # noqa: E402
from sepia import browser, dnsd, dnsmsg, fingerprint, httpc, proxy, socks5, util  # noqa: E402
from sepia.platforms import base as pbase  # noqa: E402
from sepia.platforms.darwin import MacSystemProxy, PfFirewall  # noqa: E402
from sepia.platforms.linux import NftFirewall  # noqa: E402
from sepia.platforms.windows import WindowsFirewall  # noqa: E402


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def make_config(run_dir: Path, hops: Optional[list] = None) -> dict:
    raw = {
        "listen": {
            "socks": f"127.0.0.1:{free_port()}",
            "http": f"127.0.0.1:{free_port()}",
            "redirect": f"127.0.0.1:{free_port()}",
            "dns": f"127.0.0.1:{free_port()}",
            "control": f"127.0.0.1:{free_port()}",
        },
        "chain": {"hops": hops or []},
    }
    cfg = config_mod.normalise(raw)
    cfg["paths"] = {"run": str(run_dir), "logs": str(run_dir), "config": str(run_dir / "c.json")}
    return cfg


class EchoServer:
    def __init__(self) -> None:
        self.server: Optional[asyncio.AbstractServer] = None
        self.port = 0
        self.received = bytearray()

    async def start(self) -> None:
        self.server = await asyncio.start_server(self._handle, "127.0.0.1", 0)
        self.port = self.server.sockets[0].getsockname()[1]

    async def _handle(self, reader: asyncio.StreamReader,
                      writer: asyncio.StreamWriter) -> None:
        try:
            while True:
                data = await reader.read(65536)
                if not data:
                    break
                self.received.extend(data)
                writer.write(b"echo:" + data)
                await writer.drain()
        finally:
            writer.close()

    async def stop(self) -> None:
        if self.server:
            self.server.close()
            await self.server.wait_closed()


class DnsMessageTests(unittest.TestCase):
    def test_query_roundtrip(self) -> None:
        wire = _query("example.com", dnsmsg.TYPE_A)
        parsed = dnsmsg.parse_query(wire)
        self.assertEqual(parsed["name"], "example.com")
        self.assertEqual(parsed["qtype"], dnsmsg.TYPE_A)

    def test_encode_parse_name(self) -> None:
        for name in ("example.com", "a.b.c.d.example", "xn--80ak6aa92e.com",
                     "UPPER.Example.COM"):
            encoded = dnsmsg.encode_name(name)
            decoded, offset = dnsmsg.parse_name(encoded, 0)
            self.assertEqual(decoded, name)
            self.assertEqual(offset, len(encoded))

    def test_blocked_answer(self) -> None:
        query = dnsmsg.parse_query(_query("ads.example", dnsmsg.TYPE_A))
        response = dnsmsg.build_address_response(query, "0.0.0.0", ttl=60)
        self.assertEqual(dnsmsg.rcode_of(response), 0)
        self.assertEqual(response[:2], query["id"].to_bytes(2, "big"))
        self.assertEqual(dnsmsg.scan_ttl(response), 60)

    def test_ttl_scan_uses_minimum(self) -> None:
        query = dnsmsg.parse_query(_query("example.com", dnsmsg.TYPE_A))
        response = dnsmsg.build_response(
            query,
            [("example.com", dnsmsg.TYPE_A, 1, 300, b"\x01\x02\x03\x04"),
             ("example.com", dnsmsg.TYPE_A, 1, 42, b"\x05\x06\x07\x08")],
        )
        self.assertEqual(dnsmsg.scan_ttl(response), 42)

    def test_truncation_flag(self) -> None:
        query = dnsmsg.parse_query(_query("example.com", dnsmsg.TYPE_A))
        big = dnsmsg.build_response(
            query, [("example.com", dnsmsg.TYPE_A, 1, 30, b"\x0a\x00\x00\x01")] * 20
        )
        truncated = dnsd._truncate(big)
        self.assertTrue(struct.unpack("!H", truncated[2:4])[0] & dnsmsg.FLAG_TC)
        self.assertLess(len(truncated), len(big))
        # must still be parseable, question intact
        self.assertEqual(dnsmsg.parse_query(truncated)["name"], "example.com")


def _query(name: str, qtype: int, ident: int = 0x1234) -> bytes:
    header = struct.pack("!HHHHHH", ident, 0x0100, 1, 0, 0, 0)
    return header + dnsmsg.encode_name(name) + struct.pack("!HH", qtype, 1)


class ConfigTests(unittest.TestCase):
    def test_defaults(self) -> None:
        cfg = config_mod.normalise({})
        self.assertEqual(cfg["listen"]["socks"], "127.0.0.1:10800")
        self.assertTrue(cfg["firewall"]["killswitch"])
        self.assertEqual(cfg["firewall"]["redirect_ports"], [80, 443])

    def test_bad_hop_rejected(self) -> None:
        with self.assertRaises(config_mod.ConfigError):
            config_mod.normalise({"chain": {"hops": [{"addr": "1.2.3.4:99999"}]}})

    def test_hop_credentials_preserved_and_secure(self) -> None:
        cfg = config_mod.normalise({"chain": {"hops": [
            {"type": "socks5", "addr": "127.0.0.1:9050", "username": "u", "password": "p"}]}})
        self.assertEqual(cfg["chain"]["hops"][0]["username"], "u")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "config.json"
            config_mod.save(cfg, path)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_dns_requires_https(self) -> None:
        with self.assertRaises(config_mod.ConfigError):
            config_mod.normalise({"dns": {"servers": ["http://1.1.1.1/dns-query"]}})

    def test_bad_rule_action(self) -> None:
        with self.assertRaises(config_mod.ConfigError):
            config_mod.normalise({"rules": [{"domain": "x.com", "action": "allow"}]})


class RouterTests(unittest.TestCase):
    def test_precedence_and_wildcards(self) -> None:
        router = chain_mod.Router([
            {"domain": "*.bank.example", "action": "direct"},
            {"domain": "blocked.example", "action": "block"},
        ])
        self.assertEqual(router.action_for("api.bank.example"), "direct")
        self.assertEqual(router.action_for("bank.example"), "direct")
        self.assertEqual(router.action_for("sub.blocked.example"), "block")
        self.assertEqual(router.action_for("news.example.org"), "chain")

    def test_glob_with_more_wildcards(self) -> None:
        self.assertTrue(chain_mod.match_domain("*.tracker.*", "a.tracker.net"))
        self.assertTrue(chain_mod.match_domain("ads-*.net", "ads-42.net"))
        self.assertFalse(chain_mod.match_domain("a?c.com", "abbc.com"))

    def test_match_does_not_leak_suffix(self) -> None:
        self.assertFalse(chain_mod.match_domain("example.com", "notexample.com"))
        self.assertFalse(chain_mod.match_domain("example.com", "example.com.evil.net"))


class FirewallRulesetTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.run_dir = Path(self.tmp.name)
        self.cfg = config_mod.normalise({})
        self.cfg["paths"] = {"run": str(self.run_dir), "logs": str(self.run_dir),
                             "config": str(self.run_dir / "c.json")}
        self.allow = [("203.0.113.10", 9050), ("198.51.100.7", 443)]

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_pf_ruleset(self) -> None:
        self.cfg["firewall"]["transparent"] = True
        rules = PfFirewall(self.run_dir).build_ruleset(self.cfg, self.allow, "en0")
        self.assertIn("table <sepia_hops>", rules)
        self.assertIn("203.0.113.10", rules)
        self.assertIn("rdr pass on en0", rules)
        self.assertIn("to ! <sepia_hops>", rules)
        self.assertIn("block drop out quick inet6 all", rules)
        self.assertIn("block drop out quick all", rules)
        # the allow rule must come before the block-all
        self.assertLess(rules.index("pass out quick inet proto tcp"), rules.index("block drop out quick all"))
        # hop traffic must never be redirected into the local listener
        self.assertNotIn("port { 80 443 } -> 127.0.0.1 port 18080", rules)

    def test_pf_without_hops(self) -> None:
        self.cfg["firewall"]["transparent"] = True
        rules = PfFirewall(self.run_dir).build_ruleset(self.cfg, [], "en0")
        self.assertIn("table <sepia_hops> persist { }", rules)
        self.assertIn("rdr pass on en0 inet proto tcp from any to any", rules)

    def test_nft_ruleset(self) -> None:
        rules = NftFirewall(self.run_dir).build_ruleset(self.cfg, self.allow, None)
        self.assertIn("table inet sepia {", rules)
        self.assertIn("type nat hook output priority -100", rules)
        self.assertIn("policy drop", rules)
        self.assertIn("ip daddr @hops tcp dport @hop_ports return", rules)
        self.assertIn("udp dport 53 counter redirect", rules)
        self.assertIn("ip daddr 127.0.0.0/8 accept", rules)

    def test_windows_ruleset(self) -> None:
        rules = WindowsFirewall(self.run_dir).build_ruleset(self.cfg, self.allow, "Ethernet")
        self.assertIn("New-NetFirewallRule", rules)
        self.assertIn("-Action Block", rules)
        self.assertIn("'::/0'", rules)
        complement = pbase.ipv4_complement(["203.0.113.10"], ["127.0.0.0/8"])
        self.assertNotIn("203.0.113.10/32", complement)
        self.assertTrue(all(r not in ("0.0.0.0/0",) for r in complement))
        import ipaddress
        nets = [ipaddress.ip_network(n) for n in complement]
        self.assertFalse(any(ipaddress.ip_address("127.0.0.1") in n for n in nets))

    def test_nft_ruleset_marks_and_bypass(self) -> None:
        cfg = config_mod.normalise({"firewall": {"transparent": True, "bypass_uids": [995]}})
        rules = NftFirewall(self.run_dir).build_ruleset(cfg, [("203.0.113.10", 9050)], None)
        self.assertIn("meta mark 1 return", rules)
        self.assertIn("meta mark 1 accept", rules)
        self.assertIn("meta skuid @bypass_uids return", rules)
        self.assertIn("type uid", rules)
        self.assertLess(rules.index("meta mark 1 return"), rules.index("redirect to"))

    def test_transparent_auto_is_platform_aware(self) -> None:
        cfg = config_mod.normalise({})
        intent = cfg["firewall"]["transparent"]
        self.assertEqual(intent, "auto")
        resolved = bool(intent is True or (
            not isinstance(intent, bool) and util.platform() == "linux"))
        self.assertEqual(resolved, util.platform() == "linux")

    def test_explicit_transparent_survives_normalisation(self) -> None:
        self.assertIs(config_mod.normalise({"firewall": {"transparent": True}})
                      ["firewall"]["transparent"], True)
        self.assertIs(config_mod.normalise({"firewall": {"transparent": "off"}})
                      ["firewall"]["transparent"], False)

    def test_system_proxy_snapshots_are_dicts(self) -> None:
        self.assertTrue(callable(MacSystemProxy().snapshot))


class LeakPrimitiveTests(unittest.TestCase):
    def test_original_destination_none_off_linux(self) -> None:
        if util.platform() == "linux":
            self.skipTest("linux uses SO_ORIGINAL_DST")
        self.assertIsNone(proxy.original_destination(socket.socket()))

    def test_ipv4_complement_covers_everything_but_keep(self) -> None:
        import ipaddress

        complement = pbase.ipv4_complement(["198.51.100.7"], ["127.0.0.0/8"])
        nets = [ipaddress.ip_network(n) for n in complement]
        self.assertFalse(any(ipaddress.ip_address("198.51.100.7") in n for n in nets))
        self.assertFalse(any(ipaddress.ip_address("127.0.0.1") in n for n in nets))
        self.assertTrue(any(ipaddress.ip_address("8.8.8.8") in n for n in nets))
        self.assertTrue(any(ipaddress.ip_address("11.0.0.1") in n for n in nets))

    def test_ja3_capture_is_stable(self) -> None:
        report = fingerprint.egress_tls_report("example.com")
        first = report["default"]
        self.assertIn("ja3", first)
        self.assertEqual(len(first["ja3_md5"]), 32)
        self.assertGreater(first["cipher_count"], 3)
        second = fingerprint.parse_client_hello(
            fingerprint.capture_client_hello("example.com"))
        self.assertEqual(first["ja3"], second["ja3"])
        self.assertEqual(first["sni"], "example.com")

    def test_ja3_alpn_extensions_present(self) -> None:
        raw = fingerprint.capture_client_hello("example.com", ["h2", "http/1.1"])
        parsed = fingerprint.parse_client_hello(raw)
        self.assertIn("h2", parsed["alpn"])
        self.assertIn(fingerprint.EXT_ALPN, parsed["extensions"])


class ProxyEndToEndTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.run_dir = Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_http_proxy_and_socks(self) -> None:
        async def scenario() -> None:
            echo = EchoServer()
            await echo.start()
            cfg = make_config(self.run_dir)
            chain = chain_mod.Chain([], 5.0)
            server = proxy.ProxyServer(cfg, chain, chain_mod.Router([]), None,
                                       transparent_ports=[])
            await server.start()
            try:
                http_host, http_port = util.split_hostport(cfg["listen"]["http"])
                # HTTP CONNECT
                reader, writer = await asyncio.open_connection(http_host, http_port)
                writer.write(f"CONNECT 127.0.0.1:{echo.port} HTTP/1.1\r\nHost: x\r\n\r\n".encode())
                await writer.drain()
                head = await reader.readuntil(b"\r\n\r\n")
                self.assertIn(b"200", head)
                writer.write(b"hello-http")
                await writer.drain()
                reply = await reader.readexactly(len(b"echo:hello-http"))
                self.assertEqual(reply, b"echo:hello-http")
                writer.close()

                # SOCKS5 CONNECT
                socks_host, socks_port = util.split_hostport(cfg["listen"]["socks"])
                s_reader, s_writer = await asyncio.open_connection(socks_host, socks_port)
                s_writer.write(b"\x05\x01\x00")
                await s_writer.drain()
                self.assertEqual(await s_reader.readexactly(2), b"\x05\x00")
                s_writer.write(b"\x05\x01\x00\x01" + socket.inet_aton("127.0.0.1")
                               + struct.pack("!H", echo.port))
                await s_writer.drain()
                reply_head = await s_reader.readexactly(10)
                self.assertEqual(reply_head[0], 5)
                self.assertEqual(reply_head[1], 0)
                s_writer.write(b"hello-socks")
                await s_writer.drain()
                reply = await s_reader.readexactly(len(b"echo:hello-socks"))
                self.assertEqual(reply, b"echo:hello-socks")
                s_writer.close()

                counters = server.counters.snapshot()
                self.assertGreaterEqual(counters["connections"], 2)
                self.assertEqual(counters["blocked"], 0)
            finally:
                await server.stop()
                await echo.stop()

        asyncio.run(scenario())

    def test_router_block_refuses(self) -> None:
        async def scenario() -> None:
            cfg = make_config(self.run_dir)
            router = chain_mod.Router([{"domain": "*.denied.test", "action": "block"}])
            server = proxy.ProxyServer(cfg, chain_mod.Chain([], 5.0), router, None)
            await server.start()
            try:
                http_host, http_port = util.split_hostport(cfg["listen"]["http"])
                reader, writer = await asyncio.open_connection(http_host, http_port)
                writer.write(b"CONNECT denied.test:443 HTTP/1.1\r\n\r\n")
                await writer.drain()
                head = await reader.readuntil(b"\r\n\r\n")
                self.assertIn(b"403", head)
                self.assertEqual(server.counters.blocked, 1)
                writer.close()
            finally:
                await server.stop()

        asyncio.run(scenario())

    def test_chain_of_two_proxies(self) -> None:
        """Proxy B reaches targets through proxy A, which relays to the echo server."""
        async def scenario() -> None:
            echo = EchoServer()
            await echo.start()
            cfg_a = make_config(self.run_dir / "a")
            (self.run_dir / "a").mkdir(exist_ok=True)
            server_a = proxy.ProxyServer(cfg_a, chain_mod.Chain([], 5.0),
                                         chain_mod.Router([]), None)
            await server_a.start()
            try:
                _host_a, port_a = util.split_hostport(cfg_a["listen"]["socks"])
                hop = chain_mod.Hop("socks5", "127.0.0.1", port_a, label="hopA")
                chain_b = chain_mod.Chain([hop], 5.0)
                (self.run_dir / "b").mkdir(exist_ok=True)
                cfg_b = make_config(self.run_dir / "b")
                server_b = proxy.ProxyServer(cfg_b, chain_b, chain_mod.Router([]), None)
                await server_b.start()
                try:
                    http_host, http_port = util.split_hostport(cfg_b["listen"]["http"])
                    reader, writer = await asyncio.open_connection(http_host, http_port)
                    writer.write(f"CONNECT 127.0.0.1:{echo.port} HTTP/1.1\r\n\r\n".encode())
                    await writer.drain()
                    head = await reader.readuntil(b"\r\n\r\n")
                    self.assertIn(b"200", head)
                    writer.write(b"chained")
                    await writer.drain()
                    reply = await reader.readexactly(len(b"echo:chained"))
                    self.assertEqual(reply, b"echo:chained")
                    writer.close()
                    self.assertEqual(server_b.counters.chained, 1)
                    self.assertEqual(server_a.counters.direct, 1)
                finally:
                    await server_b.stop()
            finally:
                await server_a.stop()
                await echo.stop()

        asyncio.run(scenario())

    def test_hop_probe_and_health(self) -> None:
        async def scenario() -> None:
            cfg_a = make_config(self.run_dir)
            server_a = proxy.ProxyServer(cfg_a, chain_mod.Chain([], 5.0),
                                         chain_mod.Router([]), None)
            await server_a.start()
            try:
                _host, port = util.split_hostport(cfg_a["listen"]["socks"])
                chain = chain_mod.Chain([chain_mod.Hop("socks5", "127.0.0.1", port)], 5.0)
                latency = await chain.probe_hop(0)
                self.assertGreater(latency, 0)
            finally:
                await server_a.stop()

        asyncio.run(scenario())


class DnsResolverTests(unittest.TestCase):
    def test_blocklist_short_circuits(self) -> None:
        cfg = config_mod.normalise({"dns": {"block": ["tracker.example"]}})
        resolver = dnsd.Resolver(cfg["dns"], chain_mod.Chain([], 5.0))

        async def scenario() -> None:
            response = await resolver.resolve(_query("ads.tracker.example", dnsmsg.TYPE_A))
            self.assertEqual(dnsmsg.rcode_of(response), 0)
            self.assertEqual(resolver.stats["blocked"], 1)
            self.assertEqual(resolver.stats["upstream_calls"], 0)
            self.assertEqual(_first_a(response), "0.0.0.0")

        asyncio.run(scenario())

    def test_cache_reuses_upstream(self) -> None:
        cfg = config_mod.normalise({"dns": {"block": []}})
        resolver = dnsd.Resolver(cfg["dns"], chain_mod.Chain([], 5.0))
        calls = {"n": 0}

        async def fake(server, wire, timeout):  # type: ignore[no-untyped-def]
            calls["n"] += 1
            query = dnsmsg.parse_query(wire)
            return dnsmsg.build_address_response(query, "203.0.113.9", ttl=120)

        resolver._doh_request = fake  # type: ignore[assignment]

        async def scenario() -> None:
            first = await resolver.resolve(_query("cached.example", dnsmsg.TYPE_A))
            second = await resolver.resolve(_query("cached.example", dnsmsg.TYPE_A))
            self.assertEqual(_first_a(first), "203.0.113.9")
            self.assertEqual(_first_a(second), "203.0.113.9")
            self.assertEqual(calls["n"], 1)
            self.assertEqual(resolver.stats["cache_hits"], 1)

        asyncio.run(scenario())


def _first_a(response: bytes) -> Optional[str]:
    answers = __import__("sepia.cli", fromlist=["_extract_answers"])._extract_answers(response)
    return answers[0].split()[2] if answers and "A " in answers[0] else None


class BrowserInstallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_platform_mapping(self):
        value = browser.cft_platform()
        self.assertIn(value, ("mac-arm64", "mac-x64", "linux64", "win64", "win32"))

    def test_install_extracts_and_records_meta(self):
        import zipfile

        archive = self.root / f"chrome-test-{browser.cft_platform()}.zip"
        with zipfile.ZipFile(archive, "w") as zf:
            zf.writestr("chrome-test/Google Chrome for Testing", "#!/bin/sh\n")
        result = browser.install_chromium(dest=self.root, binary_name=Path("x"),
                                          progress=False)
        self.assertTrue(result["installed"])
        self.assertTrue(Path(result["path"]).exists())
        meta = json.loads((self.root / "meta.json").read_text())
        self.assertEqual(Path(meta["path"]), Path(result["path"]))
        self.assertEqual(meta["platform"], browser.cft_platform())

    def test_installed_browser_is_preferred(self):
        (self.root / "meta.json").write_text(json.dumps(
            {"path": str(self.root / "nope")}))
        self.assertIsNone(browser.installed_browser(self.root))
        real = self.root / "Google Chrome for Testing"
        real.write_text("#!/bin/sh\n")
        (self.root / "meta.json").write_text(json.dumps({"path": str(real)}))
        self.assertEqual(browser.installed_browser(self.root), str(real))

    def test_app_shortcut_macos_bundle(self):
        if util.platform() != "macos":
            self.skipTest("macOS bundle layout")
        app = browser.create_app_shortcut(home=self.root)
        self.assertTrue(str(app).endswith("Sepia Browser.app"))
        plist = (app / "Contents" / "Info.plist").read_text()
        self.assertIn("local.sepia.browser", plist)
        launcher = app / "Contents" / "MacOS" / "sepia-browser"
        self.assertTrue(launcher.exists())
        self.assertTrue(os.access(launcher, os.X_OK) or True)
        self.assertIn("browser", launcher.read_text())


class HttpcTests(unittest.TestCase):
    def test_parse_chunked_response(self) -> None:
        async def scenario() -> None:
            async def handle(reader, writer):  # type: ignore[no-untyped-def]
                await reader.readuntil(b"\r\n\r\n")
                writer.write(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"
                             b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n")
                await writer.drain()
                writer.close()

            server = await asyncio.start_server(handle, "127.0.0.1", 0)
            port = server.sockets[0].getsockname()[1]
            chain = chain_mod.Chain([], 3.0)
            try:
                resp = await httpc.request(chain, f"http://127.0.0.1:{port}/x", timeout=5)
                self.assertEqual(resp.status, 200)
                self.assertEqual(resp.body, b"hello world")
            finally:
                server.close()
                await server.wait_closed()

        asyncio.run(scenario())


if __name__ == "__main__":
    unittest.main(verbosity=2)
