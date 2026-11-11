"""Daemon lifecycle: bind listeners, run until signalled, pid file, background spawn."""
from __future__ import annotations

import asyncio
import os
import signal
import sys
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

from . import chain as chain_mod
from . import config as config_mod
from . import dnsd, proxy, util

LOG = util.get_logger("service")

def pid_path(cfg: Dict[str, Any]) -> Path:
    return Path(str(cfg["paths"]["run"])) / f"sepia-{config_mod.instance_id(cfg)}.pid"


def read_pid(cfg: Dict[str, Any]) -> Optional[int]:
    path = pid_path(cfg)
    try:
        pid = int(path.read_text().strip())
    except (OSError, ValueError):
        return None
    return pid if _alive(pid) else None


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def _write_pid(cfg: Dict[str, Any]) -> None:
    util.atomic_write(pid_path(cfg), f"{os.getpid()}\n")


def _clear_pid(cfg: Dict[str, Any]) -> None:
    try:
        pid_path(cfg).unlink()
    except OSError:
        pass


def dns_bindings(cfg: Dict[str, Any]) -> List[tuple]:
    """Configured resolver address, plus port 53 when we may bind it."""
    host, port = util.split_hostport(str(cfg["listen"]["dns"]))
    bindings = [(host, port)]
    can_bind_53 = util.is_root()
    if can_bind_53 and port != 53:
        bindings.append((host, 53))
    return bindings


async def serve(cfg: Dict[str, Any]) -> None:
    chain = chain_mod.Chain.from_config(cfg["chain"])
    router = chain_mod.Router(cfg["rules"])
    resolver = dnsd.Resolver(cfg["dns"], chain) if cfg["dns"].get("enabled") else None

    transparent_ports = []
    if cfg["firewall"].get("transparent"):
        transparent_ports = [int(p) for p in cfg["firewall"].get("redirect_ports") or []]

    server = proxy.ProxyServer(cfg, chain, router, resolver,
                               transparent_ports=transparent_ports)
    dns_servers: List[dnsd.DnsServer] = []
    if resolver is not None:
        for host, port in dns_bindings(cfg):
            dns_servers.append(dnsd.DnsServer(host, port, resolver))

    stop = asyncio.Event()

    def _request_stop(*_args: Any) -> None:
        LOG.info("shutdown requested")
        stop.set()

    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            loop.add_signal_handler(sig, _request_stop)
        except (NotImplementedError, RuntimeError):
            pass

    await server.start()
    for dns_server in dns_servers:
        try:
            await dns_server.start()
        except OSError as exc:
            LOG.error("DNS bind %s failed: %s", dns_server.port, exc)

    _write_pid(cfg)
    LOG.info("sepia running (pid %d, chain depth %d)", os.getpid(), chain.depth)

    serve_task = asyncio.ensure_future(server.serve_forever())
    try:
        await stop.wait()
    finally:
        serve_task.cancel()
        for dns_server in dns_servers:
            await dns_server.stop()
        await server.stop()
        _clear_pid(cfg)
        LOG.info("sepia stopped")


def run_foreground(cfg: Dict[str, Any]) -> int:
    try:
        asyncio.run(serve(cfg))
    except OSError as exc:
        LOG.error("could not start: %s", exc)
        return 1
    return 0


def spawn_background(cfg: Dict[str, Any], verbose: bool = False) -> int:
    """Double-fork, detach, log to file. Returns the daemon pid."""
    log_dir = Path(str(cfg["paths"]["logs"]))
    log_dir.mkdir(parents=True, exist_ok=True)
    logfile = log_dir / "sepia.log"

    existing = read_pid(cfg)
    if existing:
        raise RuntimeError(f"already running (pid {existing})")

    sys.stdout.flush()
    sys.stderr.flush()
    child = os.fork()
    if child > 0:
        # Intermediate child exits by design once it has forked the daemon, so
        # its exit status says nothing about the daemon. Watch the pid file.
        try:
            os.waitpid(child, 0)
        except ChildProcessError:
            pass
        deadline = time.time() + 3.0
        while time.time() < deadline:
            pid = read_pid(cfg)
            if pid:
                return pid
            time.sleep(0.02)
        raise RuntimeError("daemon did not publish a pid file; see the log")

    os.setsid()
    second = os.fork()
    if second > 0:
        os._exit(0)

    os.chdir(str(Path.home()))
    os.umask(0o077)
    devnull = os.open(os.devnull, os.O_RDONLY)
    os.dup2(devnull, 0)
    log_fd = os.open(str(logfile), os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    os.dup2(log_fd, 1)
    os.dup2(log_fd, 2)
    os.close(devnull)
    os.close(log_fd)

    util.setup_logging(verbose=verbose, logfile=None)
    code = 1
    try:
        code = run_foreground(cfg)
    finally:
        os._exit(code)


def stop(cfg: Dict[str, Any], timeout: float = 5.0, force: bool = False) -> bool:
    pid = read_pid(cfg)
    if pid is None:
        _clear_pid(cfg)
        return False
    sig = signal.SIGKILL if force else signal.SIGTERM
    try:
        os.kill(pid, sig)
    except OSError:
        _clear_pid(cfg)
        return False
    deadline = time.time() + timeout
    while time.time() < deadline:
        if not _alive(pid):
            break
        time.sleep(0.05)
    else:
        if not force:
            stop(cfg, timeout=1.0, force=True)
    _clear_pid(cfg)
    return True


def core_binary() -> Optional[str]:
    """Locate the Rust data plane: env override, source checkout, then PATH."""
    import shutil

    env = util.env("CORE")
    if env and Path(env).exists():
        return env
    checkout = Path(__file__).resolve().parent.parent / "core" / "target" / "release" / "sepia-core"
    if checkout.exists():
        return str(checkout)
    return shutil.which("sepia-core")


def spawn_engine(cfg: Dict[str, Any], engine: str = "python",
                 config_path: Optional[Path] = None, verbose: bool = False) -> int:
    """Start the chosen data plane and return its pid."""
    if engine != "rust":
        return spawn_background(cfg, verbose=verbose)

    import subprocess

    binary = core_binary()
    if not binary:
        raise RuntimeError(
            "the rust engine is not built: run scripts/build.sh (or set SEPIA_CORE)")
    existing = read_pid(cfg)
    if existing:
        raise RuntimeError(f"already running (pid {existing})")
    log_dir = Path(str(cfg["paths"]["logs"]))
    log_dir.mkdir(parents=True, exist_ok=True)
    logfile = log_dir / "sepia-core.log"
    handle = open(logfile, "ab")
    process = subprocess.Popen(
        [binary, "--config", str(config_path or config_mod.config_path())],
        stdin=subprocess.DEVNULL, stdout=handle, stderr=handle,
        start_new_session=True, close_fds=True,
    )
    handle.close()
    util.atomic_write(pid_path(cfg), f"{process.pid}\n")
    return process.pid


def wait_health(cfg: Dict[str, Any], timeout: float = 6.0) -> bool:
    """Poll the control socket until the daemon answers."""
    import json
    import socket as socket_mod

    host, port = util.split_hostport(str(cfg["listen"]["control"]))
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket_mod.create_connection((host, port), timeout=0.5) as sock:
                sock.sendall(b"GET /health HTTP/1.0\r\nHost: sepia\r\n\r\n")
                data = sock.recv(4096).decode("utf-8", "replace")
            if "200 OK" in data and "true" in data:
                return True
        except OSError:
            time.sleep(0.1)
    return False
