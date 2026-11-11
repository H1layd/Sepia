"""Command line interface."""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import socket
import sys
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from . import __version__, chain as chain_mod, config as config_mod
from . import dnsd, fingerprint, httpc, service, util, verify
from .platforms import PlatformError, capabilities, get_firewall, get_system_proxy

LOG = util.get_logger("cli")


# --------------------------------------------------------------------------- #
# output helpers
# --------------------------------------------------------------------------- #


def say(message: str = "") -> None:
    print(message, flush=True)


def kv(key: str, value: Any) -> None:
    print(f"  {key:<12} {value}", flush=True)


def heading(text: str) -> None:
    print(f"\n{text}", flush=True)
    print("  " + "─" * max(10, len(text)), flush=True)


def fail(message: str, code: int = 1) -> int:
    print(f"error: {message}", file=sys.stderr, flush=True)
    return code


# --------------------------------------------------------------------------- #
# shared plumbing
# --------------------------------------------------------------------------- #


def load_cfg(args: argparse.Namespace) -> Dict[str, Any]:
    path = Path(args.config) if getattr(args, "config", None) else None
    return config_mod.load(path)


def run_dir(cfg: Dict[str, Any]) -> Path:
    return Path(str(cfg["paths"]["run"]))


def runtime_path(cfg: Dict[str, Any]) -> Path:
    return run_dir(cfg) / f"runtime-{config_mod.instance_id(cfg)}.json"


def local_listeners(cfg: Dict[str, Any]) -> List[Tuple[str, int]]:
    out = []
    for key in ("socks", "http", "redirect", "dns", "control"):
        out.append(util.split_hostport(str(cfg["listen"][key])))
    return out


def hop_points_at_self(cfg: Dict[str, Any], hop_addr: str) -> bool:
    host, port = util.split_hostport(hop_addr)
    loopback = {"127.0.0.1", "localhost", "::1", "0.0.0.0"}
    for listen_host, listen_port in local_listeners(cfg):
        if port != listen_port:
            continue
        if host == listen_host:
            return True
        if host in loopback and listen_host in loopback:
            return True
    return False


def resolve_allowlist(cfg: Dict[str, Any]) -> List[Tuple[str, int]]:
    """Hop endpoints, resolved before the killswitch closes the doors."""
    entries: List[Tuple[str, int]] = []
    for hop in cfg["chain"]["hops"]:
        host, port = util.split_hostport(str(hop["addr"]))
        if util.is_ip(host):
            entries.append((host, port))
            continue
        addresses = util.resolve_v4(host)
        if not addresses:
            raise PlatformError(f"cannot resolve hop {host!r} for the allowlist")
        for address in addresses:
            entries.append((address, port))
    return sorted(set(entries))


def control_get(cfg: Dict[str, Any], path: str = "/status",
                timeout: float = 3.0) -> Optional[Dict[str, Any]]:
    host, port = util.split_hostport(str(cfg["listen"]["control"]))
    try:
        with socket.create_connection((host, port), timeout=timeout) as sock:
            sock.sendall(f"GET {path} HTTP/1.0\r\nHost: sepia\r\n\r\n".encode())
            chunks = []
            while True:
                data = sock.recv(65536)
                if not data:
                    break
                chunks.append(data)
    except OSError:
        return None
    raw = b"".join(chunks)
    _head, _, body = raw.partition(b"\r\n\r\n")
    try:
        return json.loads(body.decode("utf-8", "replace"))
    except ValueError:
        return None


def build_chain(cfg: Dict[str, Any]) -> chain_mod.Chain:
    return chain_mod.Chain.from_config(cfg["chain"])


# --------------------------------------------------------------------------- #
# commands
# --------------------------------------------------------------------------- #


def cmd_init(args: argparse.Namespace) -> int:
    try:
        path = config_mod.init(force=args.force)
    except config_mod.ConfigError as exc:
        return fail(str(exc))
    say(f"config written: {path}")
    say("next: add your hops under chain.hops, then run  sudo sepia up")
    return 0


def cmd_config(args: argparse.Namespace) -> int:
    if args.action == "path":
        say(str(config_mod.config_path()))
        return 0
    try:
        cfg = load_cfg(args)
    except config_mod.ConfigError as exc:
        return fail(str(exc))
    if args.action == "validate":
        say("config valid")
        caps = capabilities(run_dir(cfg))
        heading("platform")
        kv("os", caps.get("platform"))
        fw = caps.get("firewall") or {}
        kv("firewall", f"{fw.get('name')} (available={fw.get('available')})")
        kv("transparent", f"{fw.get('transparent')} scope={fw.get('transparent_scope')}")
        kv("interface", fw.get("interface"))
        sp = caps.get("system_proxy") or {}
        kv("system proxy", f"{sp.get('name')} (available={sp.get('available')})")
        return 0
    if args.action == "show":
        say(json.dumps(cfg, indent=2))
        return 0
    return fail("unknown action")


def cmd_chain(args: argparse.Namespace) -> int:
    try:
        cfg = load_cfg(args)
    except config_mod.ConfigError as exc:
        return fail(str(exc))
    chain = build_chain(cfg)

    async def probe() -> int:
        heading(f"chain: {chain.depth} hop(s)")
        if not chain.hops:
            kv("mode", "direct (no hops configured)")
        ok_all = True
        for index, hop in enumerate(chain.hops):
            try:
                latency = await chain.probe_hop(index)
                kv(f"hop{index + 1}", f"{hop.describe()}  {latency * 1000:.0f}ms  reachable")
            except Exception as exc:  # noqa: BLE001
                ok_all = False
                kv(f"hop{index + 1}", f"{hop.describe()}  UNREACHABLE ({type(exc).__name__}: {exc})")
        health = await chain.health(str(cfg["chain"].get("health_url")), timeout=15)
        heading("exit")
        if health.get("ok"):
            kv("exit ip", health.get("exit_ip"))
            kv("country", health.get("country"))
            kv("latency", f"{health.get('latency_ms')}ms")
        else:
            ok_all = False
            kv("failed", health.get("error"))
        return 0 if ok_all else 1

    return asyncio.run(probe())


def cmd_dns(args: argparse.Namespace) -> int:
    try:
        cfg = load_cfg(args)
    except config_mod.ConfigError as exc:
        return fail(str(exc))
    chain = build_chain(cfg)

    async def probe() -> int:
        resolver = dnsd.Resolver(cfg["dns"], chain)
        from . import dnsmsg

        wire = _build_query(args.name, args.type)
        started = time.monotonic()
        try:
            response = await resolver.resolve(wire, timeout=args.timeout)
        except Exception as exc:  # noqa: BLE001
            return fail(f"resolution failed: {exc}")
        elapsed = (time.monotonic() - started) * 1000
        if not response:
            return fail("empty response")
        rcode = dnsmsg.rcode_of(response)
        kv("name", args.name)
        kv("rcode", rcode)
        kv("elapsed", f"{elapsed:.0f}ms")
        answers = _extract_answers(response)
        for answer in answers:
            kv("answer", answer)
        if not answers:
            say("  (no answer records)")
        return 0 if rcode == 0 else 1

    return asyncio.run(probe())


def _build_query(name: str, qtype_name: str = "A") -> bytes:
    from . import dnsmsg

    qtype = {v: k for k, v in dnsmsg.TYPE_NAMES.items()}.get(qtype_name.upper(), dnsmsg.TYPE_A)
    import struct

    header = struct.pack("!HHHHHH", util.int_id(), 0x0100, 1, 0, 0, 0)
    return header + dnsmsg.encode_name(name) + struct.pack("!HH", qtype, 1)


def _extract_answers(data: bytes) -> List[str]:
    from . import dnsmsg

    try:
        _ident, _flags, qdcount, ancount, _nscount, _arcount = __import__("struct").unpack(
            "!HHHHHH", data[:12])
        offset = 12
        for _ in range(qdcount):
            _name, offset = dnsmsg.parse_name(data, offset)
            offset += 4
        out = []
        for _ in range(ancount):
            name, offset = dnsmsg.parse_name(data, offset)
            rtype, _rclass, ttl, rdlen = __import__("struct").unpack(
                "!HHIH", data[offset:offset + 10])
            offset += 10
            rdata = data[offset:offset + rdlen]
            offset += rdlen
            typename = dnsmsg.TYPE_NAMES.get(rtype, str(rtype))
            if rtype == dnsmsg.TYPE_A and rdlen == 4:
                out.append(f"{name} {typename} {socket.inet_ntoa(rdata)} ttl={ttl}")
            elif rtype == dnsmsg.TYPE_AAAA and rdlen == 16:
                out.append(f"{name} {typename} {socket.inet_ntop(socket.AF_INET6, rdata)} ttl={ttl}")
            elif rtype == dnsmsg.TYPE_CNAME:
                target, _ = dnsmsg.parse_name(data, offset - rdlen)
                out.append(f"{name} CNAME {target}")
            else:
                out.append(f"{name} {typename} {rdlen}B ttl={ttl}")
        return out
    except Exception:  # noqa: BLE001 - display only
        return []


def _rollback(cfg: Dict[str, Any], backend: Optional[Any], system_proxy: Optional[Any],
              runtime: Dict[str, Any]) -> List[str]:
    """Undo every lockdown step, in reverse order. Returns human-readable notes."""
    notes: List[str] = []
    if isinstance(system_proxy, object) and runtime.get("system_proxy"):
        try:
            system_proxy.restore((runtime.get("system_proxy") or {}).get("snapshot") or {})
            notes.append("system proxy restored")
        except Exception as exc:  # noqa: BLE001
            notes.append(f"system proxy restore failed: {exc}")
    if backend is not None:
        try:
            backend.teardown()
            notes.append("firewall removed")
        except Exception as exc:  # noqa: BLE001
            notes.append(f"firewall teardown failed: {exc}")
    if service.stop(cfg, force=True):
        notes.append("daemon stopped")
    try:
        runtime_path(cfg).unlink()
    except OSError:
        pass
    return notes


def cmd_up(args: argparse.Namespace) -> int:
    try:
        cfg = load_cfg(args)
    except config_mod.ConfigError as exc:
        return fail(str(exc))

    if args.no_firewall:
        cfg["firewall"]["enabled"] = False
        cfg["firewall"]["transparent"] = False
    if args.no_transparent:
        cfg["firewall"]["transparent"] = False
    if args.system_proxy:
        cfg["system_proxy"]["enabled"] = True
    if args.no_system_proxy:
        cfg["system_proxy"]["enabled"] = False

    running = service.read_pid(cfg)
    if running:
        return fail(f"already running (pid {running}); run 'sepia down' first")

    needs_root = (cfg["firewall"]["enabled"] or cfg["system_proxy"]["enabled"]
                  or util.split_hostport(str(cfg["listen"]["dns"]))[1] < 1024)
    if needs_root and not util.is_root() and not args.dry_run:
        return fail("this profile needs root: run  sudo sepia up")

    if (cfg["firewall"]["enabled"] and cfg["firewall"]["killswitch"]
            and not cfg["chain"]["hops"]):
        return fail(
            "killswitch needs at least one hop: with no hops there is nothing to "
            "allow through and the machine will go dark.\n"
            "       add chain.hops, or run 'sepia up --no-firewall'."
        )

    for hop in cfg["chain"]["hops"]:
        if hop_points_at_self(cfg, str(hop["addr"])):
            return fail(
                f"hop {hop['label']} ({hop['addr']}) points back at this very "
                f"instance — the chain would recurse forever.\n"
                f"       start a separate hop instance with its own control port."
            )

    try:
        allowlist = resolve_allowlist(cfg)
    except PlatformError as exc:
        return fail(str(exc))

    heading("sepia up")
    kv("chain", f"{len(cfg['chain']['hops'])} hop(s)")
    for hop in cfg["chain"]["hops"]:
        kv("", util.redact_hostport(f"{hop['label']} {hop['type']} {hop['addr']}"))
    if not cfg["chain"]["hops"]:
        kv("warning", "no hops configured — traffic is forwarded directly")
    kv("socks", cfg["listen"]["socks"])
    kv("http", cfg["listen"]["http"])
    kv("dns", cfg["listen"]["dns"])

    caps = capabilities(run_dir(cfg))
    backend: Optional[Any] = None
    system_proxy: Optional[Any] = None
    if cfg["firewall"]["enabled"]:
        try:
            backend = get_firewall(run_dir(cfg))
        except PlatformError as exc:
            return fail(str(exc))
        if not backend.available():
            return fail(f"firewall backend {backend.name} not available")
        kv("firewall", f"{backend.name} (transparent={backend.transparent_scope})")
    if cfg["system_proxy"]["enabled"]:
        try:
            system_proxy = get_system_proxy(run_dir(cfg))
            if not system_proxy.available():
                return fail(f"system proxy backend {system_proxy.name} not available")
        except PlatformError as exc:
            return fail(str(exc))

    transparent_intent = cfg["firewall"].get("transparent", "auto")
    if not isinstance(transparent_intent, bool):
        # "auto": full transparent interception only where it is safe
        # (Linux recovers the original destination; pf on Darwin cannot).
        transparent_intent = bool(
            backend is not None and backend.supports_transparent
            and backend.transparent_scope == "all-tcp"
        )
    cfg["firewall"]["transparent"] = bool(transparent_intent)
    if cfg["firewall"]["transparent"]:
        kv("transparent", "on (all TCP)")
    elif cfg["firewall"].get("enabled"):
        kv("transparent", "off (system proxy + SOCKS only)")

    if args.foreground:
        return service.run_foreground(cfg)

    engine = getattr(args, "engine", "python") or "python"
    if engine == "rust" and cfg["firewall"].get("transparent"):
        return fail("the rust engine has no transparent redirect listener yet; "
                    "use --no-transparent or --engine python")
    if engine == "rust" and not service.core_binary():
        return fail("the rust engine is not built: run scripts/build.sh")

    try:
        pid = service.spawn_engine(
            cfg, engine,
            config_path=Path(args.config) if getattr(args, "config", None) else None,
            verbose=args.verbose,
        )
    except RuntimeError as exc:
        return fail(str(exc))
    kv("engine", engine)
    kv("daemon", f"pid {pid}")
    if not service.wait_health(cfg, timeout=8):
        service.stop(cfg, force=True)
        return fail("daemon did not become healthy; see ~/.sepia/logs/sepia.log")
    kv("health", "ok")

    runtime: Dict[str, Any] = {
        "pid": pid,
        "engine": engine,
        "started": int(time.time()),
        "config": str(config_mod.config_path()),
        "allowlist": allowlist,
        "system_proxy": None,
        "firewall": None,
        "dns_bindings": service.dns_bindings(cfg),
    }

    if system_proxy is not None:
        try:
            snapshot = system_proxy.snapshot()
            http_host, http_port = util.split_hostport(str(cfg["listen"]["http"]))
            socks_host, socks_port = util.split_hostport(str(cfg["listen"]["socks"]))
            system_proxy.apply(
                http_host, http_port, socks_host, socks_port,
                dns_host="127.0.0.1" if cfg["dns"]["enabled"] else None,
            )
            runtime["system_proxy"] = {"backend": system_proxy.name, "snapshot": snapshot}
            kv("system proxy", f"applied via {system_proxy.name}")
        except Exception as exc:  # noqa: BLE001
            LOG.warning("system proxy failed: %s", exc)
            kv("system proxy", f"FAILED: {exc}")

    if backend is not None and not args.dry_run and not args.force:
        health = asyncio.run(build_chain(cfg).health(
            str(cfg["chain"].get("health_url")), timeout=15))
        if not health.get("ok"):
            service.stop(cfg, force=True)
            return fail(
                f"chain is unhealthy ({health.get('error')}); refusing to lock the "
                f"network down.\n       fix the chain, or force it: sepia up --force"
            )
        kv("chain health", f"ok via {health.get('exit_ip')} ({health.get('latency_ms')}ms)")

    if backend is not None:
        iface = cfg["firewall"].get("iface")
        if iface in (None, "", "auto") or cfg["firewall"].get("auto_iface"):
            iface = backend.detect_interface() or iface
        rules = backend.build_ruleset(cfg, allowlist, iface)
        rules_file = run_dir(cfg) / f"{backend.name.split()[0].replace('(', '')}-preview.conf"
        try:
            util.atomic_write(rules_file, rules)
        except OSError:
            pass
        if args.dry_run:
            heading(f"ruleset ({backend.name}, interface {iface})")
            say(rules)
        else:
            try:
                backend.apply(rules)
                runtime["firewall"] = {"backend": backend.name, "interface": iface,
                                       "rules_file": str(rules_file)}
                kv("firewall", f"applied on {iface}")
            except (PlatformError, OSError) as exc:
                for note in _rollback(cfg, None, system_proxy, runtime):
                    kv("rollback", note)
                return fail(f"firewall apply failed: {exc}")

            # A local hop, a "direct" rule or a broken mark can turn the
            # redirect into a loop. Prove the path still works, or unwind.
            time.sleep(1.5)
            if control_get(cfg, "/health") is None:
                notes = _rollback(cfg, backend, system_proxy, runtime)
                for note in notes:
                    kv("rollback", note)
                return fail("daemon lost its listeners after lockdown; rolled back")
            recheck = asyncio.run(build_chain(cfg).health(
                str(cfg["chain"].get("health_url")), timeout=15))
            if not recheck.get("ok"):
                notes = _rollback(cfg, backend, system_proxy, runtime)
                for note in notes:
                    kv("rollback", note)
                return fail(
                    "the chain stopped working after lockdown (redirect loop? "
                    "a local hop needs firewall.bypass_uids). rolled back; network is safe."
                )
            kv("post-check", f"chain ok, exit {recheck.get('exit_ip')}")

    util.write_json(runtime_path(cfg), runtime)
    if args.dry_run:
        service.stop(cfg, force=True)
        return 0

    heading("ready")
    kv("dns", "127.0.0.1:53 (system resolver)" if util.is_root() else cfg["listen"]["dns"])
    kv("next", "sepia verify")
    kv("if broken", "sudo sepia panic")
    return 0


def cmd_down(args: argparse.Namespace) -> int:
    try:
        cfg = load_cfg(args)
    except config_mod.ConfigError as exc:
        return fail(str(exc))
    runtime = util.read_json(runtime_path(cfg))
    heading("sepia down")

    sysinfo = runtime.get("system_proxy")
    if sysinfo:
        try:
            backend = get_system_proxy(run_dir(cfg))
            backend.restore(sysinfo.get("snapshot") or {})
            kv("system proxy", "restored")
        except Exception as exc:  # noqa: BLE001
            kv("system proxy", f"restore failed: {exc}")

    fwinfo = runtime.get("firewall")
    stale = False
    if not fwinfo and util.is_root():
        try:
            stale = bool(get_firewall(run_dir(cfg)).status().get("rules_installed"))
        except Exception:  # noqa: BLE001
            stale = False
    if fwinfo or stale:
        try:
            backend = get_firewall(run_dir(cfg))
            backend.teardown()
            kv("firewall", "removed")
        except Exception as exc:  # noqa: BLE001
            kv("firewall", f"teardown failed: {exc}")

    stopped = service.stop(cfg)
    kv("daemon", "stopped" if stopped else "was not running")
    try:
        runtime_path(cfg).unlink()
    except OSError:
        pass
    say("\nnetwork restored")
    return 0


def cmd_panic(args: argparse.Namespace) -> int:
    try:
        cfg = load_cfg(args)
    except config_mod.ConfigError as exc:
        return fail(str(exc))
    heading("panic")
    runtime = util.read_json(runtime_path(cfg))
    for key, action in (
        ("system_proxy", lambda: get_system_proxy(run_dir(cfg)).restore(
            (runtime.get("system_proxy") or {}).get("snapshot") or {})),
        ("firewall", lambda: get_firewall(run_dir(cfg)).teardown()),
    ):
        try:
            action()
            kv(key, "reset")
        except Exception as exc:  # noqa: BLE001
            kv(key, f"failed: {exc}")
    kv("daemon", "killed" if service.stop(cfg, force=True) else "not running")
    try:
        cfg_path = config_mod.config_path()
        cfg_path.exists() and util.secure_file(cfg_path)
    except OSError:
        pass
    stale_files = [runtime_path(cfg), run_dir(cfg) / "pf-snapshot.conf",
                   run_dir(cfg) / f"state-{config_mod.instance_id(cfg)}.json"]
    stale_files += list(run_dir(cfg).glob("runtime-*.json")) if args.all else []
    for stale in stale_files:
        try:
            stale.unlink()
        except OSError:
            pass
    if args.wipe_logs:
        log_dir = Path(str(cfg["paths"]["logs"]))
        for logfile in log_dir.glob("*.log"):
            try:
                logfile.write_text("")
            except OSError:
                pass
        kv("logs", "wiped")
    say("\ntraffic path is back to normal. no residue left in pf/nft/WinINET.")
    return 0


def cmd_status(args: argparse.Namespace) -> int:
    try:
        cfg = load_cfg(args)
    except config_mod.ConfigError as exc:
        return fail(str(exc))
    payload = control_get(cfg, "/status")
    if payload is None:
        heading("sepia status")
        kv("daemon", "not running")
        return 1
    counters = payload.get("counters") or {}
    heading("sepia status")
    kv("pid", payload.get("pid"))
    kv("engine", payload.get("engine", "python"))
    kv("uptime", util.human_duration(float(counters.get("uptime_s") or 0)))
    kv("chain", f"{payload.get('chain_depth')} hop(s)")
    for hop in payload.get("hops") or []:
        kv("", hop)
    kv("socks", (payload.get("listen") or {}).get("socks"))
    kv("http", (payload.get("listen") or {}).get("http"))
    kv("transparent", payload.get("transparent_ports") or "disabled")
    heading("traffic")
    kv("connections", counters.get("connections"))
    kv("active", counters.get("active"))
    kv("chained", counters.get("chained"))
    kv("direct", counters.get("direct"))
    kv("blocked", counters.get("blocked"))
    kv("failed", counters.get("failed"))
    kv("upload", counters.get("bytes_up_h"))
    kv("download", counters.get("bytes_down_h"))
    dns_info = payload.get("dns") or {}
    if dns_info:
        heading("dns")
        kv("upstream", ", ".join(dns_info.get("servers") or []))
        kv("cache", f"{dns_info.get('cache_entries')} entries")
        kv("queries", dns_info.get("queries"))
        kv("cache hits", dns_info.get("cache_hits"))
        kv("blocked", dns_info.get("blocked"))
    routes = (counters.get("per_route") or {})
    if routes:
        heading("top destinations")
        for route, count in list(routes.items())[:10]:
            kv(route.split(":")[0], f"{route.split(':')[1]} x{count}")
    errors = counters.get("errors") or []
    if errors:
        heading("recent errors")
        for error in errors[-5:]:
            kv(error.get("where"), error.get("error")[:100])
    return 0


def cmd_verify(args: argparse.Namespace) -> int:
    try:
        cfg = load_cfg(args)
    except config_mod.ConfigError as exc:
        return fail(str(exc))
    chain = build_chain(cfg)
    verifier = verify.Verifier(cfg, chain, run_dir(cfg))
    try:
        results = asyncio.run(verifier.run())
    except KeyboardInterrupt:
        return 130
    if args.json:
        say(json.dumps(results, indent=2))
    else:
        say(verify.render(results))
    hard = [f for f in results.get("findings") or [] if f["level"] == "fail"]
    return 0 if not hard else 1


def cmd_audit(args: argparse.Namespace) -> int:
    report = fingerprint.audit()
    if args.json:
        say(json.dumps(report, indent=2))
        return 0
    heading("egress fingerprint")
    for label, tls in (report.get("tls_egress") or {}).items():
        if "ja3_md5" in tls:
            kv(label, f"{tls['ja3_md5']}  {tls['cipher_count']} ciphers  "
                      f"{tls['extension_count']} ext  sni={tls.get('sni')}")
        else:
            kv(label, tls.get("error"))
    heading("hostname")
    for surface in report.get("hostname_surfaces") or []:
        kv(surface["surface"], f"{surface['value']}  ({surface['note']})")
    ipv6 = report.get("ipv6") or {}
    heading("IPv6")
    kv("global", ", ".join(ipv6.get("global_ipv6") or []) or "none")
    kv("link-local", f"{len(ipv6.get('link_local_ipv6') or [])} address(es)")
    kv("leaky", ipv6.get("leaky"))
    dns = report.get("dns") or {}
    heading("system resolver")
    kv("servers", ", ".join(dns.get("servers") or []) or "unknown")
    kv("loopback only", dns.get("loopback_only"))
    stack = report.get("tcp_stack") or {}
    if stack:
        heading("tcp/ip stack")
        for key, info in stack.items():
            kv(key.split(".", 2)[-1], f"{info['value']}  — {info['note']}")
    listeners = report.get("locals") or []
    if listeners:
        heading("listening sockets")
        for line in listeners[:12]:
            say("  " + line)
    return 0


def cmd_browser(args: argparse.Namespace) -> int:
    try:
        cfg = load_cfg(args)
    except config_mod.ConfigError as exc:
        if not getattr(args, "native", False):
            return fail(str(exc))
        cfg = config_mod.normalise({})
    from . import browser as browser_mod

    offline_ok = (args.install_chromium or args.app_shortcut
                  or args.list_personas or args.no_proxy or args.native)
    if not offline_ok:
        if control_get(cfg, "/health", timeout=1.0) is None:
            return fail("the chain daemon is not running; start it with 'sepia up' "
                        "or pass --no-proxy for a direct (non-anonymous) browser")
    return browser_mod.run(cfg, args)


def probe_binary() -> Optional[str]:
    import shutil

    env = util.env("PROBE")
    if env and Path(env).exists():
        return env
    root = Path(__file__).resolve().parent.parent
    for candidate in (root / "native" / "sepiaprobe", Path.home() / ".sepia" / "bin" / "sepiaprobe"):
        if candidate.exists():
            return str(candidate)
    return shutil.which("sepiaprobe")


def cmd_probe(args: argparse.Namespace) -> int:
    import subprocess

    binary = probe_binary()
    if not binary:
        return fail("sepiaprobe is not built; run scripts/build.sh (needs a C++17 compiler)")
    argv = [binary, "--sni", args.sni, "--mode", args.mode, "--repeat", str(args.repeat)]
    if args.send:
        argv.append("--send")
    if args.json:
        argv.append("--json")
    through = args.through
    if through == "local":
        try:
            cfg = load_cfg(args)
        except config_mod.ConfigError as exc:
            return fail(str(exc))
        through = str(cfg["listen"]["socks"])
    if through:
        argv.extend(["--through", through])
    proc = subprocess.run(argv)
    return proc.returncode


def cmd_selftest(args: argparse.Namespace) -> int:
    try:
        cfg = load_cfg(args)
    except config_mod.ConfigError as exc:
        return fail(str(exc))
    verify_obj = verify.Verifier(cfg, build_chain(cfg), run_dir(cfg))
    heading("enforcement self-test")

    async def run_checks() -> int:
        res = await verify_obj.enforcement()
        through = res["through_proxy"]
        direct = res["direct_http"]
        open_port = res["open_port"]

        kv("exit via proxy", through.get("ip") or f"failed: {through.get('error')}")
        kv("direct request exits as", direct.get("ip") or f"blocked: {direct.get('error')}")
        kv("same exit", res.get("same_exit"))
        kv("random port 8.8.8.8:853", "open (LEAK)" if open_port.get("ok") else "denied")
        kv("plaintext dns", "intercepted by sepia" if res.get("dns_intercepted")
           else ("answered directly (LEAK)" if res.get("dns_leak") else "no answer"))
        kv("local resolver", "answering" if res["local_dns"].get("ok")
           else f"silent: {res['local_dns'].get('error')}")

        problems = []
        if not through.get("ok"):
            problems.append("the proxy path is broken")
        if res.get("leak"):
            problems.append("direct traffic leaves by a different address than the chain")
        if open_port.get("ok"):
            problems.append("a port outside the redirect allowlist still reaches the internet")
        if res.get("dns_leak"):
            problems.append("plaintext DNS escapes the resolver")
        if not res["local_dns"].get("ok"):
            problems.append("the local resolver does not answer")
        if problems:
            say("")
            for problem in problems:
                say(f"  FAIL  {problem}")
            return 1
        say("\n  enforcement holds: only the chain can reach the network")
        return 0

    return asyncio.run(run_checks())


# --------------------------------------------------------------------------- #
# parser
# --------------------------------------------------------------------------- #


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="sepia",
        description="layered traffic anonymisation: chain + killswitch + DoH + leak proof",
    )
    parser.add_argument("--version", action="version", version=f"sepia {__version__}")
    parser.add_argument("-v", "--verbose", action="store_true", help="debug logging")
    parser.add_argument("--config", help="path to config.json")
    sub = parser.add_subparsers(dest="command", required=True)

    p_init = sub.add_parser("init", help="write a default config")
    p_init.add_argument("--force", action="store_true")
    p_init.set_defaults(func=cmd_init)

    p_cfg = sub.add_parser("config", help="inspect or validate the config")
    p_cfg.add_argument("action", choices=["show", "validate", "path"])
    p_cfg.set_defaults(func=cmd_config)

    p_chain = sub.add_parser("chain", help="probe every hop and the exit")
    p_chain.set_defaults(func=cmd_chain)

    p_dns = sub.add_parser("dns", help="resolve a name through the DoH path")
    p_dns.add_argument("name")
    p_dns.add_argument("-t", "--type", default="A")
    p_dns.add_argument("--timeout", type=float, default=10.0)
    p_dns.set_defaults(func=cmd_dns)

    p_up = sub.add_parser("up", help="start the daemon and lock the network down")
    p_up.add_argument("--foreground", action="store_true")
    p_up.add_argument("--no-firewall", action="store_true")
    p_up.add_argument("--no-transparent", action="store_true")
    p_up.add_argument("--system-proxy", action="store_true", help="apply OS proxy settings")
    p_up.add_argument("--no-system-proxy", action="store_true")
    p_up.add_argument("--dry-run", action="store_true", help="print ruleset, do not install")
    p_up.add_argument("--engine", choices=["python", "rust"], default="python",
                      help="data plane: python (default) or the rust core")
    p_up.add_argument("--force", action="store_true",
                      help="lock the network down even if the chain looks dead")
    p_up.set_defaults(func=cmd_up)

    p_down = sub.add_parser("down", help="restore network and stop the daemon")
    p_down.set_defaults(func=cmd_down)

    p_panic = sub.add_parser("panic", help="hard reset everything sepia touched")
    p_panic.add_argument("--wipe-logs", action="store_true")
    p_panic.add_argument("--all", action="store_true",
                         help="also clear runtime files of every instance")
    p_panic.set_defaults(func=cmd_panic)

    p_status = sub.add_parser("status", help="live counters and routes")
    p_status.set_defaults(func=cmd_status)

    p_verify = sub.add_parser("verify", help="run the leak battery")
    p_verify.add_argument("--json", action="store_true")
    p_verify.set_defaults(func=cmd_verify)

    p_audit = sub.add_parser("audit", help="fingerprint and host leak surface")
    p_audit.add_argument("--json", action="store_true")
    p_audit.set_defaults(func=cmd_audit)

    p_browser = sub.add_parser("browser", help="launch a hardened, proxy-only browser")
    p_browser.add_argument("url", nargs="*", help="pages to open")
    p_browser.add_argument("--chrome", help="path to a Chromium-based binary")
    p_browser.add_argument("--seed", type=int, help="reproduce a specific persona")
    p_browser.add_argument("--country", help="pretend the exit is in this country")
    p_browser.add_argument("--timezone", help="override the persona timezone")
    p_browser.add_argument("--lang", help="comma separated accept-languages")
    p_browser.add_argument("--no-geo", action="store_true",
                           help="do not look up the exit country")
    p_browser.add_argument("--no-proxy", action="store_true",
                           help="launch without the chain (for comparison)")
    p_browser.add_argument("--strict-webrtc", action="store_true",
                           help="disable WebRTC entirely")
    p_browser.add_argument("--persist", action="store_true", help="keep the profile")
    p_browser.add_argument("--profile-dir", help="profile directory when --persist")
    p_browser.add_argument("--width", type=int)
    p_browser.add_argument("--height", type=int)
    p_browser.add_argument("--extra", action="append", help="extra chrome flag")
    p_browser.add_argument("--selftest", action="store_true",
                           help="headless run: compare the real surface with the persona")
    p_browser.add_argument("--json", action="store_true")
    p_browser.add_argument("--list-personas", action="store_true",
                           help="show the country -> locale table")
    p_browser.add_argument("--install-chromium", action="store_true",
                           help="download a private Chrome for Testing into ~/.sepia/browser")
    p_browser.add_argument("--channel", default="Stable", help="Chrome channel for --install-chromium")
    p_browser.add_argument("--force-install", action="store_true", help="re-download even if cached")
    p_browser.add_argument("--native", action="store_true",
                           help="run the from-scratch browser (Rust) instead of Chromium")
    p_browser.add_argument("--dump", help="with --native: render the page to a PNG")
    p_browser.add_argument("--text", action="store_true",
                           help="with --native: print the laid-out text")
    p_browser.add_argument("--scroll", type=float, help="with --native: vertical offset")
    p_browser.add_argument("--via", choices=["tor", "i2p", "none"],
                           help="with --native: route through the local Tor SOCKS or I2P proxy")
    p_browser.add_argument("--insecure", action="store_true",
                           help="with --native: accept self-signed certs (Gemini is TOFU)")
    p_browser.add_argument("--app", dest="app_shortcut", action="store_true",
                           help="create an application shortcut (macOS .app / .desktop / .cmd)")
    p_browser.set_defaults(func=cmd_browser)

    p_probe = sub.add_parser("probe", help="inspect the TLS ClientHello we emit (JA3)")
    p_probe.add_argument("--sni", default="example.com")
    p_probe.add_argument("--mode", choices=["chrome", "firefox"], default="chrome")
    p_probe.add_argument("--through", help="'local' for this chain's SOCKS, or host:port")
    p_probe.add_argument("--send", action="store_true", help="send the hello and read the reply")
    p_probe.add_argument("--repeat", type=int, default=1)
    p_probe.add_argument("--json", action="store_true")
    p_probe.set_defaults(func=cmd_probe)

    p_self = sub.add_parser("selftest", help="prove the killswitch actually blocks")
    p_self.set_defaults(func=cmd_selftest)
    return parser


def main(argv: Optional[List[str]] = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    util.setup_logging(verbose=getattr(args, "verbose", False),
                       logfile=None)
    try:
        return int(args.func(args))
    except KeyboardInterrupt:
        say("\ninterrupted")
        return 130
    except (config_mod.ConfigError, PlatformError) as exc:
        return fail(str(exc))
    except Exception as exc:  # noqa: BLE001 - top-level guard
        LOG.debug("unhandled", exc_info=True)
        return fail(f"{type(exc).__name__}: {exc}")


if __name__ == "__main__":
    sys.exit(main())
