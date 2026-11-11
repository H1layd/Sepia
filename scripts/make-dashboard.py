#!/usr/bin/env python3
"""Regenerate docs/anon-dashboard.png from the browser's own audit output.

The dashboard is rendered by sepia-browser itself: it is a real page, not a
screenshot of someone else's browser.
"""
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BROWSER = ROOT / "browser" / "target" / "release" / "sepia-browser"
OUT = ROOT / "docs" / "anon-dashboard.png"


def audit_facts() -> dict:
    result = subprocess.run([str(BROWSER), "--audit"], capture_output=True, text=True, timeout=60)
    facts = {"ip": "?", "loc": "?", "chain": "direct"}
    for line in result.stdout.splitlines():
        line = line.strip()
        if line.startswith("ip="):
            facts["ip"] = line.split("=", 1)[1]
        elif line.startswith("loc="):
            facts["loc"] = line.split("=", 1)[1]
        elif line.startswith("transport") and "chain" in line:
            facts["chain"] = line.split("transport", 1)[1].strip()
    return facts


HTML = """<!doctype html>
<html><head><title>Sepia anonymity dashboard</title>
<style>
 body {{ background-color: #10131a; color: #d8dbe3; font-size: 16px; }}
 h1 {{ color: #79b8ff; font-size: 30px; }}
 h2 {{ color: #ffd479; font-size: 20px; }}
 .ok {{ color: #7ee787; font-weight: bold; }}
 .k {{ color: #79b8ff; font-weight: bold; }}
 .row {{ background-color: #171b24; border: 1px solid #242a36; padding: 10px; margin: 6px 0; width: 820px; }}
 code {{ background-color: #1f2430; color: #ffd479; }}
 .foot {{ color: #6b7280; font-size: 13px; }}
</style></head><body>
<h1>Sepia · anonymity dashboard</h1>
<p>Rendered by the from-scratch engine: own HTTP client, own parser, own layout,
own rasteriser. Nothing on this page was produced by JavaScript — there is no
JavaScript.</p>
<h2>what a site can see</h2>
<div class="row"><span class="k">exit address</span> &nbsp; {ip} &nbsp; <span class="k">country</span> &nbsp; {loc}</div>
<div class="row"><span class="k">user agent</span> &nbsp; <code>sepia-browser/0.1</code></div>
<div class="row"><span class="k">headers</span> &nbsp; Host, User-Agent, Accept, Accept-Encoding, Accept-Language, Connection</div>
<div class="row"><span class="k">transport</span> &nbsp; {chain}</div>
<div class="row"><span class="k">dns</span> &nbsp; remote at the hop, DoH inside the browser for direct paths</div>
<h2>absent by construction</h2>
<div class="row"><span class="ok">no javascript</span> — no canvas, WebGL, audio, fonts, timers, workers</div>
<div class="row"><span class="ok">no cookies</span> — never sent, never stored, never replayed</div>
<div class="row"><span class="ok">no webrtc</span> — no UDP at all, so no ICE candidates and no local IPs</div>
<div class="row"><span class="ok">no disk state</span> — no cache, no profile, no history on disk</div>
<div class="row"><span class="ok">no client hints</span> — and no favicon or prefetch traffic</div>
<h2>the cost of that</h2>
<div class="row">Pages that need JavaScript render their static shell only. A site behind a
JS challenge will look empty. That is the price of not running an interpreter:
every tracker that needs script execution gets nothing.</div>
<p class="foot">Sepia 1.0 · anonymous by construction, small on purpose</p>
</body></html>
"""


def main() -> int:
    if not BROWSER.exists():
        print(f"build the browser first: {BROWSER}", file=sys.stderr)
        return 1
    facts = audit_facts()
    tmp = Path(tempfile.mkdtemp(prefix="sepia-dash-"))
    try:
        page = tmp / "dash.html"
        page.write_text(HTML.format(**facts), encoding="utf-8")
        with tempfile.TemporaryDirectory() as server_dir:
            shutil.copy(page, Path(server_dir) / "dash.html")
            import http.server
            import socketserver
            import threading

            class Handler(http.server.SimpleHTTPRequestHandler):
                def log_message(self, *args):
                    pass

            server = socketserver.TCPServer(("127.0.0.1", 0), lambda *a, **k: Handler(*a, directory=server_dir, **k))
            port = server.server_address[1]
            threading.Thread(target=server.serve_forever, daemon=True).start()
            try:
                subprocess.run(
                    [str(BROWSER), "--url", f"http://127.0.0.1:{port}/dash.html",
                     "--dump", str(OUT), "--width", "900", "--height", "1000"],
                    check=True, timeout=120,
                )
            finally:
                server.shutdown()
        print(f"wrote {OUT}  (exit {facts['ip']}, {facts['loc']})")
        return 0
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
