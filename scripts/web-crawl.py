#!/usr/bin/env python3
"""Сбор по вебу: чего движку не хватает, по частоте на всём вебе.

Берёт N случайных доменов из топа Tranco (не витрину), открывает каждый
нашим браузером с датчиками (SEPIA_REPORT, browser/src/report.rs) и
складывает отказы: «неизвестное CSS-свойство X — на 41% сайтов». Сверху
рейтинга то, что чинить первым: это исправляет больше всего сайтов сразу.

    python3 scripts/web-crawl.py --count 200 --seed 1
    python3 scripts/web-crawl.py --count 50 --top 1000 --jobs 8
    python3 scripts/web-crawl.py --compare out/crawl-a.json out/crawl-b.json

Список: ~/.cache/sepia/top-1m.csv (https://tranco-list.eu/top-1m.csv.zip),
скачивается сам, если его нет.
"""

from __future__ import annotations

import argparse
import concurrent.futures as futures
import io
import json
import os
import random
import re
import subprocess
import sys
import tempfile
import time
import urllib.request
import zipfile
from collections import defaultdict
from pathlib import Path
from typing import Dict, List

HERE = Path(__file__).resolve().parent
DEFAULT_BINARY = HERE.parent / "browser" / "target" / "release" / "sepia-browser"
CACHE = Path.home() / ".cache" / "sepia"
TRANCO = "https://tranco-list.eu/top-1m.csv.zip"

# группы для отчёта: что меняет вид страницы, что нет
GROUPS = [
    ("css-prop", "CSS-свойства, которых движок не знает"),
    ("css-value", "CSS-значения, которые движок не прочёл"),
    ("css-selector", "Селекторы, которые парсер отверг"),
    ("css-at", "@-правила, которые пропущены"),
    ("js-missing", "Глобальные имена JS, которых нет"),
    ("js-error", "Ошибки JS (нормализованные)"),
    ("js-syntax", "Синтаксис JS, который движок не разобрал"),
    ("image", "Картинки, которые не декодированы"),
    ("font", "Шрифты, которые не загружены"),
    ("load", "Отказы загрузки"),
    ("css-prop-inert", "CSS без влияния на статичный кадр (для справки)"),
]


def tranco() -> List[str]:
    path = CACHE / "top-1m.csv"
    if not path.exists():
        CACHE.mkdir(parents=True, exist_ok=True)
        print(f"скачиваю {TRANCO} …", file=sys.stderr)
        with urllib.request.urlopen(TRANCO, timeout=120) as r:
            data = r.read()
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            path.write_bytes(z.read("top-1m.csv"))
    return [line.split(",", 1)[1].strip() for line in path.read_text().splitlines() if "," in line]


def run_site(binary: Path, domain: str, timeout: float, width: int) -> Dict:
    url = f"https://{domain}/"
    with tempfile.NamedTemporaryFile(suffix=".jsonl", delete=False) as f:
        report = Path(f.name)
    env = dict(os.environ, SEPIA_REPORT=str(report))
    started = time.monotonic()
    status, text, error = "?", 0, ""
    try:
        r = subprocess.run([str(binary), "--url", url, "--text", "--width", str(width)],
                           capture_output=True, text=True, timeout=timeout, env=env)
        for line in r.stdout.splitlines():
            m = re.match(r"^(status|text nodes)\s+(\S+)", line)
            if m and m.group(1) == "status":
                status = m.group(2)
            elif m:
                text = int(m.group(2)) if m.group(2).isdigit() else 0
        if r.returncode != 0:
            error = (r.stderr.strip().splitlines() or ["exit %d" % r.returncode])[-1][:200]
    except subprocess.TimeoutExpired:
        error = "timeout"
    ms = int((time.monotonic() - started) * 1000)
    misses = []
    try:
        for line in report.read_text().splitlines():
            misses.append(json.loads(line))
    except (OSError, ValueError):
        pass
    report.unlink(missing_ok=True)
    return {"domain": domain, "status": status, "text": text, "ms": ms, "error": error, "misses": misses}


def env_failure(r: Dict) -> bool:
    return "127.0.0.1" in r["error"] and "refused" in r["error"].lower()


def summarize(results: List[Dict]) -> Dict:
    loaded = [r for r in results if not r["error"] and r["status"].startswith("2") and r["text"] > 0]
    sites: Dict[str, Dict[str, int]] = defaultdict(lambda: defaultdict(int))
    total: Dict[str, Dict[str, int]] = defaultdict(lambda: defaultdict(int))
    for r in results:
        for m in r["misses"]:
            sites[m["kind"]][m["key"]] += 1
            total[m["kind"]][m["key"]] += m["count"]
    return {
        "sites": len(results),
        "loaded": len(loaded),
        "timeouts": sum(1 for r in results if r["error"] == "timeout"),
        "failed": sum(1 for r in results if r["error"] and r["error"] != "timeout" and not env_failure(r)),
        "environment": sum(1 for r in results if env_failure(r)),
        "median_ms": sorted(r["ms"] for r in loaded)[len(loaded) // 2] if loaded else 0,
        "ranking": {kind: sorted(((k, n, total[kind][k]) for k, n in keys.items()), key=lambda t: (-t[1], -t[2], t[0]))
                    for kind, keys in sites.items()},
    }


def print_report(s: Dict, top: int) -> None:
    n = s["sites"]
    print(f"\nсайтов {n}: загружено {s['loaded']}, таймаут {s['timeouts']}, ошибка {s['failed']}, "
          f"сбой среды {s.get('environment', 0)}, "
          f"медиана загрузки {s['median_ms'] / 1000:.1f} с")
    for kind, title in GROUPS:
        rows = s["ranking"].get(kind, [])
        if not rows:
            continue
        print(f"\n## {title} [{kind}]")
        for key, count, uses in rows[:top]:
            print(f"  {100 * count / n:5.1f}%  {count:4d} сайт.  {uses:7d} раз  {key}")


def compare(a: Dict, b: Dict, top: int) -> None:
    print(f"сайтов {a['sites']} → {b['sites']}, загружено {a['loaded']} → {b['loaded']}, "
          f"медиана {a['median_ms'] / 1000:.1f} → {b['median_ms'] / 1000:.1f} с")
    for kind, title in GROUPS:
        ra = {k: c / a["sites"] for k, c, _ in a["ranking"].get(kind, [])}
        rb = {k: c / b["sites"] for k, c, _ in b["ranking"].get(kind, [])}
        keys = sorted(set(ra) | set(rb), key=lambda k: -max(ra.get(k, 0), rb.get(k, 0)))[:top]
        changed = [k for k in keys if abs(ra.get(k, 0) - rb.get(k, 0)) >= 0.01]
        if changed:
            print(f"\n## {title}")
            for k in changed:
                print(f"  {100 * ra.get(k, 0):5.1f}% → {100 * rb.get(k, 0):5.1f}%  {k}")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--count", type=int, default=100)
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--top", type=int, default=10000, help="брать из первых N доменов Tranco")
    p.add_argument("--jobs", type=int, default=6)
    p.add_argument("--timeout", type=float, default=90)
    p.add_argument("--width", type=int, default=1200)
    p.add_argument("--rows", type=int, default=25, help="строк в каждой группе")
    p.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    p.add_argument("--out", type=Path, help="куда сохранить JSON (по умолчанию out/crawl-<seed>-<count>.json)")
    p.add_argument("--compare", nargs=2, type=Path, metavar=("A", "B"))
    args = p.parse_args()

    if args.compare:
        a, b = (json.loads(path.read_text())["summary"] for path in args.compare)
        compare(a, b, args.rows * 2)
        return

    domains = random.Random(args.seed).sample(tranco()[: args.top], args.count)
    results: List[Dict] = []
    with futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        jobs = {pool.submit(run_site, args.binary, d, args.timeout, args.width): d for d in domains}
        for i, job in enumerate(futures.as_completed(jobs), 1):
            r = job.result()
            results.append(r)
            mark = r["error"] or r["status"]
            print(f"[{i:3d}/{len(domains)}] {r['domain']:32s} {r['ms'] / 1000:5.1f} с  text={r['text']:<5d} {mark}",
                  file=sys.stderr, flush=True)
    # сбой среды (локальный прокси не принимал соединения) — не отказ
    # движка: такие сайты повторяем позже, а оставшиеся считаем отдельно
    for attempt in range(3):
        env = [r for r in results if env_failure(r)]
        if not env:
            break
        print(f"повтор {len(env)} сайтов: прокси был недоступен (попытка {attempt + 1})", file=sys.stderr, flush=True)
        time.sleep(30)
        results = [r for r in results if not env_failure(r)]
        with futures.ThreadPoolExecutor(max_workers=max(1, args.jobs // 2)) as pool:
            results += list(pool.map(lambda d: run_site(args.binary, d, args.timeout, args.width), [r["domain"] for r in env]))
    summary = summarize(results)
    out = args.out or HERE.parent / "out" / f"crawl-{args.seed}-{args.count}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps({"args": {k: str(v) for k, v in vars(args).items()}, "summary": summary,
                               "results": results}, ensure_ascii=False, indent=1))
    print_report(summary, args.rows)
    print(f"\nсохранено: {out}")


if __name__ == "__main__":
    main()
