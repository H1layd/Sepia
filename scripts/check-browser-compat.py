#!/usr/bin/env python3
"""Capture Sepia's real renderer; never label a successful HTTP fetch as compatibility.

Developer tooling only: output files are explicitly requested here, not browser state.
"""
import argparse
import concurrent.futures
import datetime
import json
from pathlib import Path
import subprocess
import time

SITES = {
    "wikipedia": "https://en.wikipedia.org/wiki/Web_browser",
    "youtube": "https://www.youtube.com/",
    "github": "https://github.com/rust-lang/rust",
    "google": "https://www.google.com/search?q=rust",
    "yandex": "https://yandex.ru/search/?text=rust",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("browser/target/release/sepia-browser"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--sites", nargs="+", choices=SITES, default=list(SITES))
    parser.add_argument("--widths", nargs="+", type=int, default=[800, 1200, 1600])
    parser.add_argument("--scrolls", nargs="+", type=int, default=[0, 900])
    parser.add_argument("--jobs", type=int, default=3)
    args = parser.parse_args()
    binary = args.binary.resolve()
    args.out.mkdir(parents=True, exist_ok=True)

    def capture(case):
        site, width, scroll = case
        name = f"{site}-{width}-scroll{scroll}"
        command = [str(binary), "--url", SITES[site], "--dump", str((args.out / f"{name}.png").resolve()),
                   "--width", str(width), "--height", "900", "--scroll", str(scroll), "--timeout", "12"]
        started = time.monotonic()
        result = {"site": site, "url": SITES[site], "width": width, "scroll": scroll,
                  "visual_review": "pending", "interaction_review": "pending"}
        try:
            completed = subprocess.run(command, text=True, capture_output=True, timeout=90)
            result.update(exit=completed.returncode, stdout=completed.stdout, stderr=completed.stderr)
        except subprocess.TimeoutExpired as error:
            result.update(exit=None, error="process exceeded 90 seconds",
                          stdout=(error.stdout or b"").decode(errors="replace") if isinstance(error.stdout, bytes) else error.stdout)
        result["elapsed_ms"] = round((time.monotonic() - started) * 1000)
        (args.out / f"{name}.json").write_text(json.dumps(result, ensure_ascii=False, indent=2))
        print(f"{name}: exit={result['exit']}, {result['elapsed_ms']} ms", flush=True)
        return result

    cases = [(site, width, scroll) for site in args.sites for width in args.widths for scroll in args.scrolls]
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        results = list(pool.map(capture, cases))
    report = {"captured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "binary": str(binary), "note": "HTTP 200 is not visual or interaction acceptance.", "cases": results}
    (args.out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
    if any(result["exit"] != 0 for result in results):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
