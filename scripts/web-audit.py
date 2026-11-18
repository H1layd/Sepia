#!/usr/bin/env python3
"""Случайный веб: как браузер выглядит на сайтах, которые никто не выбирал.

Смысл — не «три памятника», а распределение. Каждый прогон берёт N сайтов из
широкого списка по сиду, снимает наш рендер и эталон Chrome, считает сходство
и функциональные метрики, печатает медиану и худших.

    python3 scripts/web-audit.py                 # 8 случайных сайтов
    python3 scripts/web-audit.py --count 12 --seed 7
    python3 scripts/web-audit.py --sites yandex google youtube
    python3 scripts/web-audit.py --no-visual     # только текст/ссылки/время
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import random
import re
import statistics
import subprocess
import time
from pathlib import Path
from typing import Any, Dict, List, Optional

HERE = Path(__file__).resolve().parent
DEFAULT_BINARY = HERE.parent / "browser" / "target" / "release" / "sepia-browser"
CHROME = Path("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")

# Широкий список: категории, а не витрина. Никакой подгонки под движок.
SITES: Dict[str, str] = {
    "wikipedia": "https://en.wikipedia.org/wiki/Web_browser",
    "github": "https://github.com/rust-lang/rust",
    "youtube": "https://www.youtube.com/",
    "google": "https://www.google.com/search?q=rust",
    "yandex": "https://yandex.ru/search/?text=rust",
    "duckduckgo": "https://lite.duckduckgo.com/lite/?q=rust",
    "hackernews": "https://news.ycombinator.com/",
    "reddit": "https://www.reddit.com/r/rust/",
    "stackoverflow": "https://stackoverflow.com/questions",
    "habr": "https://habr.com/ru/",
    "lenta": "https://lenta.ru/",
    "rbc": "https://www.rbc.ru/",
    "medium": "https://medium.com/",
    "devto": "https://dev.to/",
    "mdn": "https://developer.mozilla.org/en-US/docs/Web/JavaScript",
    "docsrs": "https://docs.rs/",
    "python": "https://www.python.org/",
    "rustlang": "https://www.rust-lang.org/",
    "gnu": "https://www.gnu.org/",
    "kernel": "https://www.kernel.org/",
    "arch": "https://archlinux.org/",
    "debian": "https://www.debian.org/",
    "bbc": "https://www.bbc.com/news",
    "reuters": "https://www.reuters.com/",
    "apnews": "https://apnews.com/",
    "theverge": "https://www.theverge.com/",
    "arstechnica": "https://arstechnica.com/",
    "wired": "https://www.wired.com/",
    "nytimes": "https://www.nytimes.com/",
    "guardian": "https://www.theguardian.com/international",
    "spiegel": "https://www.spiegel.de/",
    "lemonde": "https://www.lemonde.fr/",
    "elpais": "https://elpais.com/",
    "ria": "https://ria.ru/",
    "tass": "https://tass.ru/",
    "kommersant": "https://www.kommersant.ru/",
    "avito": "https://www.avito.ru/",
    "ozon": "https://www.ozon.ru/",
    "wildberries": "https://www.wildberries.ru/",
    "aliexpress": "https://www.aliexpress.com/",
    "amazon": "https://www.amazon.com/",
    "ebay": "https://www.ebay.com/",
    "booking": "https://www.booking.com/",
    "airbnb": "https://www.airbnb.com/",
    "twitch": "https://www.twitch.tv/",
    "vk": "https://vk.com/",
    "ok": "https://ok.ru/",
    "x": "https://x.com/",
    "instagram": "https://www.instagram.com/",
    "figma": "https://www.figma.com/",
    "notion": "https://www.notion.so/",
    "slack": "https://slack.com/",
    "discourse": "https://users.rust-lang.org/",
    "lobsters": "https://lobste.rs/",
    "slashdot": "https://slashdot.org/",
    "thenewstack": "https://thenewstack.io/",
    "smashing": "https://www.smashingmagazine.com/",
    "css-tricks": "https://css-tricks.com/",
    "webdev": "https://web.dev/",
    "cloudflare": "https://blog.cloudflare.com/",
    "stripe": "https://stripe.com/",
    "vercel": "https://vercel.com/",
    "netlify": "https://www.netlify.com/",
    "example": "https://example.com/",
}


def load_visual_diff():
    spec = importlib.util.spec_from_file_location("visual_diff", HERE / "visual-diff.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[union-attr]
    return module


def run_ours(binary: Path, url: str, width: int, height: int, timeout: float = 150) -> Dict[str, Any]:
    started = time.monotonic()
    try:
        result = subprocess.run([str(binary), "--url", url, "--text",
                                 "--width", str(width), "--height", str(height)],
                                capture_output=True, text=True, timeout=timeout)
        out, err, code = result.stdout, result.stderr, result.returncode
    except subprocess.TimeoutExpired:
        out, err, code = "", "timeout", -1
    ms = int((time.monotonic() - started) * 1000)
    fields: Dict[str, str] = {}
    for line in out.splitlines():
        m = re.match(r"^(status|links|link rects|text nodes|page height|fetched)\s+(.+)$", line.strip())
        if m:
            fields[m.group(1)] = m.group(2).strip()
    def num(key: str) -> int:
        value = fields.get(key, "0")
        m = re.search(r"\d+", value)
        return int(m.group(0)) if m else 0
    return {
        "ms": ms, "code": code,
        "status": fields.get("status", ""),
        "links": num("links"), "rects": num("link rects"), "text": num("text nodes"),
        "height": num("page height"),
        "stderr": err[-300:],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--out", type=Path, default=Path("/tmp/sepia-web-audit"))
    parser.add_argument("--count", type=int, default=8)
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument("--sites", nargs="*", default=[])
    parser.add_argument("--width", type=int, default=1200)
    parser.add_argument("--height", type=int, default=900)
    parser.add_argument("--wait", type=int, default=15000)
    parser.add_argument("--country", default="system")
    parser.add_argument("--no-visual", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    binary = args.binary.resolve()
    if not binary.exists():
        print(f"нет бинарника: {binary}")
        return 2
    args.out.mkdir(parents=True, exist_ok=True)

    if args.sites:
        names = args.sites
    else:
        seed = args.seed if args.seed is not None else random.randrange(1 << 30)
        names = random.Random(seed).sample(list(SITES), min(args.count, len(SITES)))
        args.seed = seed
    print(f"выборка: {len(names)} сайт(ов), сид {args.seed}")

    vd = None if args.no_visual else load_visual_diff()
    rows: List[Dict[str, Any]] = []
    for name in names:
        url = SITES.get(name)
        if not url:
            print(f"  {name}: неизвестный сайт")
            continue
        case_dir = args.out / name
        ours = run_ours(binary, url, args.width, args.height, timeout=150)
        row: Dict[str, Any] = {"site": name, "url": url, **ours}
        if vd is not None and ours["status"] not in ("403", "401", "429"):
            try:
                pinned = HERE.parent / "docs" / "qa" / "reference" / f"{name}.png"
                stats = vd.compare(url, args.width, args.height, case_dir, binary, CHROME, args.wait,
                                   country=args.country, reference_path=pinned if pinned.exists() else None)
                # пустой эталон = битое сравнение, а не провал движка
                blank = False
                ref_path = case_dir / "reference.png"
                if ref_path.exists():
                    try:
                        from PIL import Image
                        import numpy as np
                        a = np.asarray(Image.open(ref_path).convert("RGB"), dtype=np.float32)
                        if float((a.sum(axis=2) < 720).mean()) < 0.01:
                            blank = True
                    except Exception:  # noqa: BLE001
                        pass
                if blank:
                    row["error"] = "эталон пуст (Chrome не дождался)"
                else:
                    row.update({"visual": stats.get("score"), "layout": stats.get("block_iou"),
                                "detail": stats.get("detail_iou"), "missing": stats.get("missing_bands"),
                                "error": stats.get("error")})
            except Exception as exc:  # noqa: BLE001
                row["error"] = repr(exc)
        rows.append(row)
        score = row.get("visual")
        print(f"  {name:<14} {ours['ms']:>6} ms  text={ours['text']:<5} links={ours['links']:<5} "
              f"status={ours['status'] or '?':<4} visual={score if score is not None else '—'}")

    usable = [r for r in rows if r.get("visual") is not None]
    blocked = [r for r in rows if r["status"] in ("403", "401", "429")]
    if usable:
        scores = [r["visual"] for r in usable]
        median = statistics.median(scores)
    else:
        median = None
    print()
    print(f"медиана сходства: {median:.1f}" if median is not None else "медиана сходства: нет данных")
    if blocked:
        print(f"закрыты от автоматики (403/401/429): {', '.join(r['site'] for r in blocked)}")
    slow = sorted((r for r in rows if r["ms"] > 20000), key=lambda r: -r["ms"])
    if slow:
        print("медленнее 20 с: " + ", ".join(f"{r['site']} {r['ms']} ms" for r in slow))
    worst = sorted(usable, key=lambda r: r["visual"])[:3]
    if worst:
        print("хуже всего: " + ", ".join(f"{r['site']} {r['visual']:.1f}" for r in worst))

    report = {"generated": time.strftime("%Y-%m-%d %H:%M"), "seed": args.seed,
              "sample": names, "median_visual": median, "rows": rows}
    (args.out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2))
    print(f"отчёт: {args.out / 'report.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
