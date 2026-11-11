"""sepia browser: a Chromium instance that lies consistently.

Design rule: never tell the JS world something the wire contradicts. Chrome's
HTTP/2, TLS and Client Hints all come from one build, so the user agent and the
platform stay real; timezone, locale, screen, device scale and core count are
set natively through CDP (Emulation domain), languages through --accept-lang,
and only the surfaces Chrome cannot be told about (canvas, audio, WebGL strings,
WebRTC candidates) are shimmed at document_start.

Everything derives from one persona, so a session is internally consistent and
a new session is a different machine.

Control goes over --remote-debugging-pipe (fds 3/4), the same channel Playwright
uses: branded Chrome no longer honours --load-extension, and an extension would
have been a weaker primitive anyway (page-world injection, detectable wrapper).
"""
from __future__ import annotations

import hashlib
import http.server
import json
import os
import random
import shutil
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

from . import cdp as cdp_mod
from . import chain as chain_mod
from . import httpc, util

LOG = util.get_logger("browser")

SHIM_PATH = Path(__file__).resolve().parent / "browser_ext" / "sepia-shim.js"

GEO_TABLE: Dict[str, Tuple[List[str], List[str]]] = {
    "US": (["America/New_York", "America/Chicago", "America/Los_Angeles"], ["en-US", "en"]),
    "CA": (["America/Toronto", "America/Vancouver"], ["en-CA", "en"]),
    "GB": (["Europe/London"], ["en-GB", "en"]),
    "IE": (["Europe/Dublin"], ["en-IE", "en"]),
    "DE": (["Europe/Berlin"], ["de-DE", "de", "en"]),
    "NL": (["Europe/Amsterdam"], ["nl-NL", "nl", "en"]),
    "FR": (["Europe/Paris"], ["fr-FR", "fr"]),
    "ES": (["Europe/Madrid"], ["es-ES", "es"]),
    "PT": (["Europe/Lisbon"], ["pt-PT", "pt"]),
    "IT": (["Europe/Rome"], ["it-IT", "it"]),
    "CH": (["Europe/Zurich"], ["de-CH", "de", "fr"]),
    "AT": (["Europe/Vienna"], ["de-AT", "de"]),
    "SE": (["Europe/Stockholm"], ["sv-SE", "sv", "en"]),
    "NO": (["Europe/Oslo"], ["nb-NO", "no", "en"]),
    "DK": (["Europe/Copenhagen"], ["da-DK", "da", "en"]),
    "FI": (["Europe/Helsinki"], ["fi-FI", "fi", "en"]),
    "PL": (["Europe/Warsaw"], ["pl-PL", "pl"]),
    "CZ": (["Europe/Prague"], ["cs-CZ", "cs"]),
    "RO": (["Europe/Bucharest"], ["ro-RO", "ro"]),
    "UA": (["Europe/Kyiv"], ["uk-UA", "uk", "ru"]),
    "RU": (["Europe/Moscow", "Europe/Kaliningrad"], ["ru-RU", "ru", "en"]),
    "TR": (["Europe/Istanbul"], ["tr-TR", "tr"]),
    "AE": (["Asia/Dubai"], ["ar-AE", "ar", "en"]),
    "IL": (["Asia/Jerusalem"], ["he-IL", "he", "en"]),
    "IN": (["Asia/Kolkata"], ["en-IN", "en", "hi"]),
    "SG": (["Asia/Singapore"], ["en-SG", "en"]),
    "JP": (["Asia/Tokyo"], ["ja-JP", "ja"]),
    "KR": (["Asia/Seoul"], ["ko-KR", "ko"]),
    "HK": (["Asia/Hong_Kong"], ["zh-HK", "zh", "en"]),
    "TW": (["Asia/Taipei"], ["zh-TW", "zh"]),
    "BR": (["America/Sao_Paulo"], ["pt-BR", "pt", "en"]),
    "AR": (["America/Argentina/Buenos_Aires"], ["es-AR", "es"]),
    "MX": (["America/Mexico_City"], ["es-MX", "es"]),
    "CL": (["America/Santiago"], ["es-CL", "es"]),
    "ZA": (["Africa/Johannesburg"], ["en-ZA", "en"]),
    "AU": (["Australia/Sydney", "Australia/Melbourne"], ["en-AU", "en"]),
    "NZ": (["Pacific/Auckland"], ["en-NZ", "en"]),
    "AM": (["Asia/Yerevan"], ["hy-AM", "hy", "ru"]),
    "GE": (["Asia/Tbilisi"], ["ka-GE", "ka", "ru", "en"]),
    "KZ": (["Asia/Almaty"], ["kk-KZ", "kk", "ru"]),
    "BY": (["Europe/Minsk"], ["be-BY", "be", "ru"]),
    "MD": (["Europe/Chisinau"], ["ro-MD", "ro", "ru"]),
    "LV": (["Europe/Riga"], ["lv-LV", "lv", "ru"]),
    "LT": (["Europe/Vilnius"], ["lt-LT", "lt"]),
    "EE": (["Europe/Tallinn"], ["et-EE", "et", "ru"]),
}

SCREEN_PRESETS: List[Tuple[int, int, int]] = [
    (1920, 1080, 0), (2560, 1440, 0), (1512, 982, 25), (1440, 900, 25),
    (1680, 1050, 25), (2560, 1600, 0), (1366, 768, 40), (2880, 1800, 0),
    (3840, 2160, 0), (1600, 900, 40),
]

GPU_BY_PLATFORM: Dict[str, List[Tuple[str, str]]] = {
    "macos": [
        ("Google Inc. (Apple)", "ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)"),
        ("Google Inc. (Apple)", "ANGLE (Apple, ANGLE Metal Renderer: Apple M3 Pro, Unspecified Version)"),
        ("Google Inc. (Apple)", "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)"),
    ],
    "linux": [
        ("Google Inc. (Intel)", "ANGLE (Intel, Mesa Intel(R) UHD Graphics 620 (KBL GT2), OpenGL 4.6)"),
        ("Google Inc. (AMD)", "ANGLE (AMD, AMD Radeon RX 6600 (radeonsi, navi23), OpenGL 4.6)"),
        ("Google Inc. (NVIDIA)", "ANGLE (NVIDIA, NVIDIA GeForce GTX 1660 (nvidia), OpenGL 4.6)"),
    ],
    "win32": [
        ("Google Inc. (NVIDIA)", "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)"),
        ("Google Inc. (Intel)", "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)"),
        ("Google Inc. (AMD)", "ANGLE (AMD, AMD Radeon RX 580 Series Direct3D11 vs_5_0 ps_5_0, D3D11)"),
    ],
}

CHROME_CANDIDATES = {
    "macos": [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ],
    "linux": [
        "/usr/bin/google-chrome", "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium", "/usr/bin/chromium-browser", "/usr/bin/brave-browser",
    ],
    "win32": [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    ],
}


@dataclass
class Persona:
    seed: int
    chrome_major: str
    chrome_version: str
    platform_token: str
    webgl_vendor: str
    webgl_renderer: str
    screen_width: int
    screen_height: int
    taskbar: int
    dpr: float
    hardware_concurrency: int
    device_memory: int
    max_touch_points: int
    languages: List[str]
    timezone: str
    country: Optional[str] = None
    exit_ip: Optional[str] = None
    strict_webrtc: bool = False

    def user_agent(self) -> str:
        return (f"Mozilla/5.0 ({self.platform_token}) AppleWebKit/537.36 "
                f"(KHTML, like Gecko) Chrome/{self.chrome_major}.0.0.0 Safari/537.36")

    def platform_token_short(self) -> str:
        if self.platform_token.startswith("Macintosh"):
            return "MacIntel"
        if self.platform_token.startswith("Windows"):
            return "Win32"
        return "Linux x86_64"

    def js_config(self) -> Dict[str, Any]:
        return {
            "guard": "__sp" + format(self.seed & 0xFFFFFF, "06x"),
            "canvasSeed": self.seed & 0xFFFFFFFF,
            "audioSeed": (self.seed >> 8) & 0xFFFFFFFF,
            "webglVendor": self.webgl_vendor,
            "webglRenderer": self.webgl_renderer,
            "screen": {
                "width": self.screen_width,
                "height": self.screen_height,
                "taskbar": self.taskbar,
                "dpr": self.dpr,
                "colorDepth": 24,
            },
            "hardwareConcurrency": self.hardware_concurrency,
            "deviceMemory": self.device_memory,
            "maxTouchPoints": self.max_touch_points,
            "platform": self.platform_token_short(),
            "languages": self.languages,
            "strictWebrtc": self.strict_webrtc,
        }

    def summary(self) -> Dict[str, Any]:
        return {
            "seed": self.seed,
            "country": self.country,
            "exit_ip": self.exit_ip,
            "timezone": self.timezone,
            "languages": self.languages,
            "screen": f"{self.screen_width}x{self.screen_height}@{self.dpr}",
            "cpu_cores": self.hardware_concurrency,
            "device_memory": f"{self.device_memory}GB",
            "gpu": self.webgl_renderer,
            "user_agent": self.user_agent(),
            "strict_webrtc": self.strict_webrtc,
        }


CFT_JSON = "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json"
def cft_dir() -> Path:
    """Where the private browser lives. Overridable for tests and for users who
    want it on another disk."""
    override = util.env("CFT_DIR")
    return Path(override) if override else (Path.home() / ".sepia" / "browser")


def cft_platform() -> str:
    system = util.platform()
    machine = (os.uname().machine if hasattr(os, "uname") else "").lower()
    arch = "arm64" if machine in ("arm64", "aarch64") else "x64"
    if system == "macos":
        return f"mac-{arch}"
    if system == "linux":
        return "linux64"
    return "win64" if arch == "x64" else "win32"


def cft_latest(channel: str = "Stable", timeout: float = 20.0) -> Tuple[str, str]:
    """Resolve the newest Chrome for Testing build for this platform."""
    import urllib.request

    with urllib.request.urlopen(CFT_JSON, timeout=timeout) as response:
        data = json.loads(response.read().decode("utf-8"))
    payload = data["channels"].get(channel) or data["channels"]["Stable"]
    version = payload["version"]
    for entry in payload["downloads"]["chrome"]:
        if entry["platform"] == cft_platform():
            return version, entry["url"]
    raise RuntimeError(f"no Chrome for Testing build for {cft_platform()}")


def installed_browser(dest: Optional[Path] = None) -> Optional[str]:
    meta = (dest or cft_dir()) / "meta.json"
    try:
        data = json.loads(meta.read_text())
    except (OSError, ValueError):
        return None
    binary = Path(str(data.get("path", "")))
    return str(binary) if binary.exists() else None


def install_chromium(channel: str = "Stable", force: bool = False,
                     progress: Optional[bool] = None,
                     dest: Optional[Path] = None,
                     binary_name: Optional[Path] = None) -> Dict[str, Any]:
    """Fetch a private Chrome for Testing build so the browser is self-contained.

    The point is isolation: this build never sees the user's daily profile,
    extensions or sync state, and its location is under ~/.sepia.
    """
    import urllib.request
    import zipfile

    if progress is None:
        progress = sys.stdout.isatty()
    target_dir = dest or cft_dir()
    existing = installed_browser(target_dir)
    if existing and not force:
        meta = json.loads((target_dir / "meta.json").read_text())
        return {"installed": True, "path": existing, "version": meta.get("version"),
                "url": meta.get("url"), "cached": True}

    version = "test"
    url = "local"
    if binary_name is None:
        version, url = cft_latest(channel)
    target_dir.mkdir(parents=True, exist_ok=True)
    archive = target_dir / f"chrome-{version}-{cft_platform()}.zip"
    if binary_name is not None and not archive.exists():
        raise RuntimeError("test mode expects an archive to exist")
    if not archive.exists():
        if progress:
            print(f"downloading Chrome for Testing {version} ({cft_platform()})")
        request = urllib.request.Request(url, headers={"User-Agent": "sepia-browser/1.0"})
        with urllib.request.urlopen(request, timeout=120) as response, open(archive, "wb") as fh:
            total = int(response.headers.get("content-length") or 0)
            done = 0
            while True:
                chunk = response.read(1 << 20)
                if not chunk:
                    break
                fh.write(chunk)
                done += len(chunk)
                if progress and total:
                    pct = done * 100 // total
                    print(f"  {pct:3d}%  {done // (1 << 20)} MiB / {total // (1 << 20)} MiB",
                          end="\r", flush=True)
        if progress:
            print()

    extract_to = target_dir / f"chrome-{version}"
    if not extract_to.exists():
        if progress:
            print(f"extracting into {extract_to}")
        extract_to.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(archive) as zf:
            zf.extractall(extract_to)

    binary = _find_cft_binary(extract_to)
    if not binary:
        raise RuntimeError(f"no browser binary inside {extract_to}")
    util.write_json(target_dir / "meta.json", {
        "version": version, "url": url, "path": str(binary), "platform": cft_platform(),
    })
    try:
        binary.chmod(0o755)
    except OSError:
        pass
    return {"installed": True, "path": str(binary), "version": version,
            "url": url, "cached": False}


def _find_cft_binary(root: Path) -> Optional[Path]:
    names = ("Google Chrome for Testing", "chrome", "Chromium", "chrome.exe")
    for name in names:
        for candidate in root.rglob(name):
            if candidate.is_file():
                return candidate
    return None


def create_app_shortcut(home: Optional[Path] = None) -> Path:
    """Make it launchable like a normal application, not a terminal command."""
    home = home or Path.home()
    system = util.platform()
    if system == "macos":
        app = home / "Applications" / "Sepia Browser.app"
        macos_dir = app / "Contents" / "MacOS"
        macos_dir.mkdir(parents=True, exist_ok=True)
        root = Path(__file__).resolve().parent.parent
        plist = f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Sepia Browser</string>
  <key>CFBundleDisplayName</key><string>Sepia Browser</string>
  <key>CFBundleIdentifier</key><string>local.sepia.browser</string>
  <key>CFBundleVersion</key><string>1.0</string>
  <key>CFBundleShortVersionString</key><string>1.0</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleExecutable</key><string>sepia-browser</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"""
        (app / "Contents" / "Info.plist").write_text(plist)
        launcher = macos_dir / "sepia-browser"
        launcher.write_text(
            "#!/bin/bash\n"
            f'exec "{root}/sepia.sh" browser "$@" >> "$HOME/.sepia/logs/browser.log" 2>&1\n'
        )
        launcher.chmod(0o755)
        return app
    if system == "linux":
        desktop = home / ".local" / "share" / "applications" / "sepia-browser.desktop"
        desktop.parent.mkdir(parents=True, exist_ok=True)
        root = Path(__file__).resolve().parent.parent
        desktop.write_text(
            "[Desktop Entry]\nType=Application\nName=Sepia Browser\n"
            "Comment=Anonymous Chromium on the sepia chain\n"
            f"Exec={root}/sepia.sh browser %u\nIcon=web-browser\nTerminal=false\n"
            "Categories=Network;WebBrowser;\n"
        )
        return desktop
    shortcut = home / "Desktop" / "Sepia Browser.cmd"
    root = Path(__file__).resolve().parent.parent
    shortcut.write_text(
        "@echo off\r\n"
        f'python "{root}\\sepia\\cli.py" browser %*\r\n'
    )
    return shortcut


def find_native_browser() -> Optional[str]:
    """The from-scratch browser: our own parser, layout and rasteriser."""
    env = util.env("NATIVE_BROWSER")
    if env and Path(env).exists():
        return env
    root = Path(__file__).resolve().parent.parent
    for candidate in (root / "browser" / "target" / "release" / "sepia-browser",
                      Path.home() / ".sepia" / "bin" / "sepia-browser"):
        if candidate.exists():
            return str(candidate)
    return shutil.which("sepia-browser")


def run_native(cfg: Dict[str, Any], args: Any) -> int:
    binary = find_native_browser()
    if not binary:
        print("error: the from-scratch browser is not built.\n"
              "  cargo build --release --manifest-path browser/Cargo.toml", file=sys.stderr)
        return 1
    cmd = [binary]
    urls = list(getattr(args, "url", []) or [])
    if urls:
        cmd += ["--url", urls[0]]
    if getattr(args, "dump", None):
        cmd += ["--dump", str(args.dump)]
    if getattr(args, "text", False):
        cmd += ["--text"]
    if getattr(args, "width", None):
        cmd += ["--width", str(args.width)]
    if getattr(args, "height", None):
        cmd += ["--height", str(args.height)]
    if getattr(args, "scroll", None):
        cmd += ["--scroll", str(args.scroll)]
    if getattr(args, "via", None):
        cmd += ["--via", str(args.via)]
    if getattr(args, "insecure", False):
        cmd += ["--insecure"]
    hops = [str(hop["addr"]) for hop in cfg["chain"]["hops"]]
    if hops:
        cmd += ["--socks", ",".join(hops)]
    else:
        # No configured hops: if a local chain is up, ride it.
        socks_host, socks_port = util.split_hostport(str(cfg["listen"]["socks"]))
        try:
            with socket.create_connection((socks_host, socks_port), timeout=0.5):
                cmd += ["--socks", str(cfg["listen"]["socks"])]
        except OSError:
            pass
    print("sepia-browser (from scratch): own HTTP client, HTML, CSS, layout and raster")
    if getattr(args, "via", None):
        print(f"  via          {args.via}")
    if hops:
        print(f"  chain        {' -> '.join(hops)}")
    proc = subprocess.run(cmd)
    return proc.returncode


def find_browser(explicit: Optional[str] = None) -> Optional[str]:
    if explicit:
        return explicit if Path(explicit).exists() else None
    private = installed_browser()
    if private:
        return private
    env = util.env("BROWSER")
    if env and Path(env).exists():
        return env
    for candidate in CHROME_CANDIDATES.get(util.platform(), []):
        if Path(candidate).exists():
            return candidate
    for name in ("google-chrome", "chromium", "chrome", "brave-browser"):
        found = shutil.which(name)
        if found:
            return found
    return None


def chrome_version(binary: str) -> Tuple[str, str]:
    try:
        proc = subprocess.run([binary, "--version"], capture_output=True, timeout=10)
        text = proc.stdout.decode("utf-8", "replace").strip()
    except Exception:  # noqa: BLE001 - version detection must never abort launch
        return "126", "126.0.0.0"
    for token in text.replace("\n", " ").split():
        if token and token[0].isdigit() and "." in token:
            return token.split(".")[0], token
    return "126", "126.0.0.0"


async def detect_exit_geo(chain: chain_mod.Chain, timeout: float = 10.0
                          ) -> Tuple[Optional[str], Optional[str]]:
    try:
        info = await httpc.ip_echo(chain, "https://cloudflare.com/cdn-cgi/trace", timeout=timeout)
        country = (info.get("country") or "").upper() or None
        return country, info.get("ip")
    except Exception as exc:  # noqa: BLE001 - the persona falls back to a neutral locale
        LOG.debug("exit geo lookup failed: %s", exc)
        return None, None


def build_persona(seed: Optional[int] = None, country: Optional[str] = None,
                  exit_ip: Optional[str] = None, binary_version: Tuple[str, str] = ("126", ""),
                  strict_webrtc: bool = False, timezone: Optional[str] = None,
                  languages: Optional[List[str]] = None) -> Persona:
    rng = random.Random(seed if seed is not None else int.from_bytes(os.urandom(8), "big"))
    platform = util.platform()
    token = {
        "macos": "Macintosh; Intel Mac OS X 10_15_7",
        "win32": "Windows NT 10.0; Win64; x64",
    }.get(platform, "X11; Linux x86_64")

    zones, langs = GEO_TABLE.get((country or "").upper(), (["UTC"], ["en-US", "en"]))
    width, height, taskbar = rng.choice(SCREEN_PRESETS)
    vendor, renderer = rng.choice(GPU_BY_PLATFORM.get(platform, GPU_BY_PLATFORM["linux"]))
    return Persona(
        seed=rng.getrandbits(32),
        chrome_major=binary_version[0],
        chrome_version=binary_version[1],
        platform_token=token,
        webgl_vendor=vendor,
        webgl_renderer=renderer,
        screen_width=width,
        screen_height=height,
        taskbar=taskbar,
        dpr=2.0 if width >= 2560 else 1.0,
        hardware_concurrency=rng.choice([4, 8, 8, 12, 16]),
        device_memory=rng.choice([4, 8, 8, 16]),
        max_touch_points=0,
        languages=languages or list(langs),
        timezone=timezone or rng.choice(zones),
        country=(country or "").upper() or None,
        exit_ip=exit_ip,
        strict_webrtc=strict_webrtc,
    )


def shim_source(persona: Persona) -> str:
    template = SHIM_PATH.read_text(encoding="utf-8")
    config_json = json.dumps(persona.js_config(), separators=(",", ":"))
    source = template.replace("/*__SEPIA_CFG__*/ null", config_json)
    if config_json not in source:
        raise RuntimeError("sepia-shim.js placeholder missing")
    return source


def persona_fingerprint(persona: Persona) -> str:
    blob = json.dumps(persona.js_config(), sort_keys=True).encode()
    return hashlib.sha256(blob).hexdigest()[:16]


# --------------------------------------------------------------------------- #
# Chrome command line
# --------------------------------------------------------------------------- #


def build_flags(profile_dir: Path, persona: Persona, proxy: Optional[str],
                window: Optional[Tuple[int, int]] = None,
                extra: Optional[List[str]] = None, headless: bool = False) -> List[str]:
    width = window[0] if window else min(persona.screen_width - 60, 1440)
    height = window[1] if window else min(persona.screen_height - 140, 900)
    flags = [
        f"--user-data-dir={profile_dir}",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-sync",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-domain-reliability",
        "--disable-client-side-phishing-detection",
        "--disable-default-apps",
        "--no-pings",
        "--metrics-recording-only",
        "--disable-breakpad",
        "--disable-crash-reporter",
        "--disable-quic",
        "--disk-cache-size=1048576",
        "--media-cache-size=1048576",
        "--disable-blink-features=AutomationControlled",
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--disable-features=Translate,OptimizationHints,MediaRouter,"
        "AutofillServerCommunication,InterestFeedContentSuggestions,"
        "CalculateNativeWinOcclusion,CertificateTransparencyComponentUpdater,"
        "SegmentationPlatform,PrivacySandboxSettings4,PrivacySandboxAdsAPIs,"
        "TopicsAPI,AttributionReportingCrossAppWeb",
        f"--window-size={width},{height}",
        f"--lang={persona.languages[0]}",
        f"--accept-lang={','.join(persona.languages)}",
        "--enable-unsafe-swiftshader",
        "about:blank",
    ]
    if proxy:
        flags.append(f"--proxy-server={proxy}")
        # Belt and braces: if anything tries a direct lookup it fails closed,
        # while loopback stays resolvable so local pages and the selftest work.
        flags.append("--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE localhost , "
                     "EXCLUDE 127.0.0.1 , EXCLUDE [::1]")
        flags.append("--proxy-bypass-list=localhost;127.0.0.1;[::1]")
    if util.platform() == "linux":
        flags.append("--disable-dev-shm-usage")
    if headless:
        flags.append("--headless=new")
    if extra:
        flags.extend(extra)
    return flags


def browser_env(persona: Persona, base: Optional[Dict[str, str]] = None) -> Dict[str, str]:
    env = dict(base if base is not None else os.environ)
    env["TZ"] = persona.timezone  # native fallback if the CDP override is rejected
    env["SEPIA_SESSION"] = str(persona.seed)
    env.pop("CHROME_USER_DATA_DIR", None)
    return env


def device_metrics(persona: Persona, window: Optional[Tuple[int, int]] = None) -> Dict[str, Any]:
    width, height = window if window else (
        min(persona.screen_width - 60, 1440), min(persona.screen_height - 140, 900))
    return {
        "width": width,
        "height": height,
        "screenWidth": persona.screen_width,
        "screenHeight": persona.screen_height,
        "screenOrientation": {"type": "landscapePrimary", "angle": 90},
        "deviceScaleFactor": persona.dpr,
        "mobile": False,
    }


def wait_session(tracker: "SessionTracker", target_id: Optional[str],
                 timeout: float = 8.0) -> Optional[str]:
    """Wait until the debugger has attached to the target we just created.

    Page creation races with auto-attach: if we navigate immediately, the first
    document can execute before the shim and the Emulation overrides land. So we
    always create about:blank, wait for the session, and only then navigate.
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        with tracker.lock:
            for session_id, info in tracker.pages.items():
                if target_id and info.get("targetId") == target_id:
                    return session_id
        time.sleep(0.05)
    return None


def open_url(cdp: cdp_mod.CDP, tracker: "SessionTracker", url: str,
             timeout: float = 15.0) -> Optional[str]:
    target = cdp.send("Target.createTarget", {"url": "about:blank"}, timeout=timeout)
    target_id = target.get("targetId")
    session_id = wait_session(tracker, target_id, timeout=timeout)
    if session_id is None:
        LOG.warning("no debugger session for target %s", target_id)
        return None
    try:
        cdp.send("Page.navigate", {"url": url}, session_id=session_id, timeout=timeout)
    except cdp_mod.CDPError as exc:
        LOG.debug("navigate failed: %s", exc)
    return session_id


class SessionTracker:
    """Keeps the page sessions we are allowed to drive."""

    def __init__(self) -> None:
        self.pages: Dict[str, Dict[str, Any]] = {}
        self.lock = threading.Lock()

    def setup(self, cdp: cdp_mod.CDP, persona: Persona, shim: str,
              window: Optional[Tuple[int, int]] = None) -> Any:
        def handler(session_id: str, target_info: Dict[str, Any]) -> None:
            kind = target_info.get("type")
            if kind not in ("page", "iframe"):
                return
            cdp_mod.prepare_target(
                cdp, session_id,
                timezone=persona.timezone,
                locale=persona.languages[0],
                hardware_concurrency=persona.hardware_concurrency,
                viewport=device_metrics(persona, window),
                shim=shim,
            )
            if kind == "page":
                with self.lock:
                    entry = dict(target_info)
                    entry["sessionId"] = session_id
                    self.pages[session_id] = entry
        return handler


# --------------------------------------------------------------------------- #
# self test
# --------------------------------------------------------------------------- #

SELFTEST_HTML = """<!doctype html>
<html><head><meta charset="utf-8"><title>sepia selftest</title></head><body>
<h1>sepia selftest</h1>
<script>
window.__sepiaSelftest = null;
(async function () {
  const out = {};
  out.ua = navigator.userAgent;
  out.platform = navigator.platform;
  out.languages = navigator.languages;
  out.webdriver = navigator.webdriver;
  out.hardwareConcurrency = navigator.hardwareConcurrency;
  out.deviceMemory = navigator.deviceMemory;
  out.maxTouchPoints = navigator.maxTouchPoints;
  out.screen = [screen.width, screen.height, screen.availWidth, screen.availHeight, screen.colorDepth];
  out.dpr = window.devicePixelRatio;
  out.timezone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  out.tzOffset = new Date().getTimezoneOffset();
  out.dateString = new Date().toString();
  try {
    const c = document.createElement('canvas');
    c.width = 240; c.height = 60;
    const ctx = c.getContext('2d');
    ctx.textBaseline = 'top';
    ctx.font = '16px Arial';
    ctx.fillStyle = '#f60';
    ctx.fillRect(0, 0, 120, 30);
    ctx.fillStyle = '#069';
    ctx.fillText('sepia canvas', 4, 4);
    const data = ctx.getImageData(0, 0, 240, 60).data;
    let hash = 0;
    for (let i = 0; i < data.length; i += 97) hash = (hash * 31 + data[i]) >>> 0;
    out.canvasHash = hash;
    out.canvasDataUrlLen = c.toDataURL().length;
  } catch (e) { out.canvasError = String(e); }
  try {
    const gl = document.createElement('canvas').getContext('webgl');
    const dbg = gl && gl.getExtension('WEBGL_debug_renderer_info');
    out.webglVendor = dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) : 'n/a';
    out.webglRenderer = dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : 'n/a';
  } catch (e) { out.webglError = String(e); }
  out.webrtcAvailable = typeof window.RTCPeerConnection === 'function';
  try {
    const pc = new RTCPeerConnection({ iceServers: [] });
    pc.createDataChannel('x');
    const candidates = [];
    await new Promise(function (resolve) {
      pc.onicecandidate = function (e) {
        if (!e.candidate) return resolve();
        candidates.push(e.candidate.candidate);
      };
      pc.createOffer().then(function (o) { return pc.setLocalDescription(o); }).catch(function () { resolve(); });
      setTimeout(resolve, 2000);
    });
    out.iceCandidates = candidates;
    const ips = [];
    candidates.forEach(function (c) {
      const m = c.match(/ (\\d+\\.\\d+\\.\\d+\\.\\d+) /);
      if (m) ips.push(m[1]);
      const m6 = c.match(/ ([0-9a-f]{1,4}:[0-9a-f:]+) /i);
      if (m6) ips.push(m6[1]);
    });
    out.webrtcLocalIps = ips;
    try { pc.close(); } catch (e) {}
  } catch (e) { out.webrtcError = String(e); }
  try {
    const r = await fetch('https://1.1.1.1/cdn-cgi/trace', { cache: 'no-store' });
    const text = await r.text();
    out.exitIp = (text.match(/^ip=(.*)$/m) || [])[1] || null;
    out.exitLoc = (text.match(/^loc=(.*)$/m) || [])[1] || null;
  } catch (e) { out.exitError = String(e); }
  window.__sepiaSelftest = out;
})();
</script></body></html>
"""


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args: Any) -> None:
        pass


def _start_report_server(payload: str) -> Tuple[socketserver.TCPServer, int]:
    class Handler(_QuietHandler):
        def do_GET(self) -> None:  # noqa: N802 - http.server API
            body = payload.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    class Server(socketserver.TCPServer):
        allow_reuse_address = True

    server = Server(("127.0.0.1", 0), Handler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, port


def selftest(binary: str, persona: Persona, proxy: Optional[str],
             timeout: float = 90.0) -> Dict[str, Any]:
    """Launch headless through the same path and read the surface back over CDP."""
    server, port = _start_report_server(SELFTEST_HTML)
    profile = Path(tempfile.mkdtemp(prefix="sepia-selftest-"))
    page_url = f"http://127.0.0.1:{port}/selftest"
    started = time.monotonic()
    session: Optional[cdp_mod.CDP] = None
    try:
        flags = build_flags(profile, persona, proxy, headless=True)
        session = cdp_mod.launch(binary, profile, flags, browser_env(persona))
        tracker = SessionTracker()
        handler = tracker.setup(session, persona, shim_source(persona))
        cdp_mod.attach_all(session, handler)
        page_session = open_url(session, tracker, page_url, timeout=20)
        if page_session is None:
            return {"ok": False, "error": "the selftest target never attached"}

        surface: Optional[Dict[str, Any]] = None
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                result = session.send(
                    "Runtime.evaluate",
                    {"expression": "JSON.stringify(window.__sepiaSelftest)",
                     "returnByValue": True},
                    session_id=page_session, timeout=2.0)
            except cdp_mod.CDPError:
                time.sleep(0.15)
                continue
            value = (result.get("result") or {}).get("value")
            if value and value != "null":
                try:
                    surface = json.loads(value)
                except ValueError:
                    surface = None
                if surface:
                    break
            time.sleep(0.15)

        if surface is None:
            return {"ok": False, "error": "the selftest page never reported a surface",
                    "elapsed_s": round(time.monotonic() - started, 1)}

        checks: List[Dict[str, Any]] = []

        def check(name: str, expected: Any, actual: Any) -> None:
            checks.append({"name": name, "expected": expected, "actual": actual,
                           "ok": actual == expected})

        check("platform", persona.platform_token_short(), surface.get("platform"))
        check("screen.width", persona.screen_width, (surface.get("screen") or [None])[0])
        check("screen.height", persona.screen_height, (surface.get("screen") or [None, None])[1])
        check("devicePixelRatio", persona.dpr, surface.get("dpr"))
        check("hardwareConcurrency", persona.hardware_concurrency,
              surface.get("hardwareConcurrency"))
        check("deviceMemory", persona.device_memory, surface.get("deviceMemory"))
        check("timezone", persona.timezone, surface.get("timezone"))
        check("webglRenderer", persona.webgl_renderer, surface.get("webglRenderer"))
        check("webdriver", False, surface.get("webdriver"))

        if persona.strict_webrtc:
            check("webrtc_disabled", False, surface.get("webrtcAvailable"))
            leaks: List[str] = []
        else:
            leaks = [ip for ip in (surface.get("webrtcLocalIps") or [])
                     if not ip.startswith(("0.", "::"))]
        checks.append({"name": "webrtc_no_local_ip", "expected": [], "actual": leaks,
                       "ok": leaks == []})
        if proxy:
            checks.append({"name": "exit_ip_present", "expected": True,
                           "actual": bool(surface.get("exitIp")),
                           "ok": bool(surface.get("exitIp"))})

        return {
            "ok": all(item["ok"] for item in checks),
            "elapsed_s": round(time.monotonic() - started, 1),
            "persona": persona.summary(),
            "surface": surface,
            "checks": checks,
        }
    finally:
        if session is not None:
            session.close()
        server.shutdown()
        server.server_close()
        shutil.rmtree(profile, ignore_errors=True)


# --------------------------------------------------------------------------- #
# entry point
# --------------------------------------------------------------------------- #


def run(cfg: Dict[str, Any], args: Any) -> int:
    import asyncio

    if getattr(args, "native", False):
        return run_native(cfg, args)

    if getattr(args, "install_chromium", False):
        try:
            result = install_chromium(getattr(args, "channel", "Stable") or "Stable",
                                      force=bool(getattr(args, "force_install", False)))
        except Exception as exc:  # noqa: BLE001 - network and archive problems
            print(f"error: {exc}", file=sys.stderr)
            return 1
        print(f"chrome for testing {result['version']}")
        print(f"  path  {result['path']}")
        print(f"  cache {result['url']}{' (already present)' if result.get('cached') else ''}")
        print("  this build is private to sepia: no profile, extensions or sync from your daily browser")
        return 0

    if getattr(args, "app_shortcut", False):
        path = create_app_shortcut()
        print(f"launcher created: {path}")
        print("open it like any app; it starts the browser on the running sepia chain")
        return 0

    binary = find_browser(getattr(args, "chrome", None))
    if not binary:
        print("error: no browser found. Two options:\n"
              "  sepia browser --install-chromium   # private Chrome for Testing in ~/.sepia/browser\n"
              "  sepia browser --chrome /path/to/chrome", file=sys.stderr)
        return 1

    version = chrome_version(binary)
    chain = chain_mod.Chain.from_config(cfg["chain"])

    country = getattr(args, "country", None)
    exit_ip: Optional[str] = None
    if not getattr(args, "no_geo", False):
        country, exit_ip = asyncio.run(detect_exit_geo(chain))

    persona = build_persona(
        seed=getattr(args, "seed", None),
        country=country,
        exit_ip=exit_ip,
        binary_version=version,
        strict_webrtc=bool(getattr(args, "strict_webrtc", False)),
        timezone=getattr(args, "timezone", None),
        languages=(args.lang.split(",") if getattr(args, "lang", None) else None),
    )

    socks_host, socks_port = util.split_hostport(str(cfg["listen"]["socks"]))
    proxy = None if getattr(args, "no_proxy", False) else f"socks5://{socks_host}:{socks_port}"

    if getattr(args, "list_personas", False):
        print("country -> timezone / languages")
        for code, (zones, langs) in sorted(GEO_TABLE.items()):
            print(f"  {code}  {zones[0]:<28} {','.join(langs)}")
        return 0

    if getattr(args, "selftest", False):
        result = selftest(binary, persona, proxy)
        if not result.get("ok") and result.get("error"):
            LOG.debug("selftest retry after: %s", result.get("error"))
            result = selftest(binary, persona, proxy)
        if getattr(args, "json", False):
            print(json.dumps(result, indent=2))
        else:
            print(f"browser : {binary}")
            print(f"version : {version[1]}")
            print(f"persona : {persona_fingerprint(persona)}")
            for key, value in result.get("persona", {}).items():
                print(f"  {key:<12} {value}")
            print("\nchecks:")
            for item in result.get("checks", []):
                mark = "OK  " if item["ok"] else "FAIL"
                print(f"  [{mark}] {item['name']}: {item['actual']!r}")
            if result.get("ok"):
                print("\nresult: the real surface matches the persona")
            else:
                print(f"\nresult: FAIL ({result.get('error', 'see checks above')})")
        return 0 if result.get("ok") else 1

    persist = bool(getattr(args, "persist", False))
    if persist:
        profile = Path(str(getattr(args, "profile_dir", "") or
                           (Path.home() / ".sepia" / "browser-profile")))
        profile.mkdir(parents=True, exist_ok=True)
    else:
        profile = Path(tempfile.mkdtemp(prefix="sepia-browser-"))

    window = None
    if getattr(args, "width", None) and getattr(args, "height", None):
        window = (args.width, args.height)
    urls = list(getattr(args, "url", []) or []) or ["about:blank"]

    shim = shim_source(persona)
    flags = build_flags(profile, persona, proxy, window=window,
                        extra=list(getattr(args, "extra", []) or []))
    print(f"sepia browser · session {persona_fingerprint(persona)}")
    for key, value in persona.summary().items():
        print(f"  {key:<12} {value}")
    print(f"  proxy        {proxy or 'direct (not anonymous)'}")
    print(f"  profile      {profile}{'' if persist else ' (ephemeral)'}")

    session: Optional[cdp_mod.CDP] = None
    try:
        session = cdp_mod.launch(binary, profile, flags, browser_env(persona))
        tracker = SessionTracker()
        handler = tracker.setup(session, persona, shim, window)
        cdp_mod.attach_all(session, handler)
        for index, url in enumerate(urls):
            if index == 0:
                open_url(session, tracker, url)
            else:
                session.send("Target.createTarget", {"url": url}, timeout=15)
        print("  controls     CDP pipe: timezone, locale, screen, cores, shim")
        print("\nclose the browser window to end the session\n")
        while session.process.poll() is None:
            time.sleep(0.25)
        return session.process.returncode or 0
    except KeyboardInterrupt:
        return 130
    finally:
        if session is not None:
            session.close()
        if not persist:
            wipe_profile(profile)


def wipe_profile(profile: Path) -> None:
    """Remove the ephemeral profile, overwriting the history-bearing files first."""
    if not profile.exists():
        return
    for pattern in ("**/History", "**/Cookies", "**/Login Data", "**/Web Data",
                    "**/Favicons", "**/Top Sites", "**/Visited Links"):
        for path in profile.glob(pattern):
            try:
                size = path.stat().st_size
                path.write_bytes(os.urandom(min(size, 65536)))
            except OSError:
                pass
    shutil.rmtree(profile, ignore_errors=True)
