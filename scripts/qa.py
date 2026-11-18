#!/usr/bin/env python3
"""Оценка браузера: сценарии, проверки, счёт.

Разработческий инструмент. Он ничего не доказывает про «настоящую»
совместимость, но даёт повторяемую цифру: какие функции живут, какие нет,
и что сломалось со вчерашнего прогона.

Запуск:
    python3 scripts/qa.py                 # все сценарии, отчёт в /tmp/sepia-qa
    python3 scripts/qa.py --sites github wikipedia
    python3 scripts/qa.py --json          # только машинный вывод
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional

DEFAULT_BINARY = Path("browser/target/release/sepia-browser")


@dataclass
class Check:
    name: str
    ok: bool
    detail: str
    weight: float = 1.0
    critical: bool = False


@dataclass
class Scenario:
    site: str
    url: str
    width: int = 1200
    height: int = 900
    # функциональные ожидания
    min_text: int = 40          # текстовых узлов: страница не пустая
    min_links: int = 5          # ссылок: страница интерактивна
    max_ms: int = 15_000        # бюджет загрузки и вёрстки
    must_contain: List[str] = field(default_factory=list)
    click: Optional[tuple] = None   # (x, y) и куда должно привести
    click_link: str = ""            # найти ссылку с этим href и кликнуть по центру
    click_expect: str = ""
    # визуальная проверка по PNG: сколько пикселей отличается от фона
    min_ink: float = 0.004      # 0.4%: страница не пустая
    min_colors: int = 12        # не «скелет» из двух серых
    tags: List[str] = field(default_factory=list)


SCENARIOS: List[Scenario] = [
    Scenario("example", "https://example.com/", min_text=30, min_links=1, max_ms=6_000,
             must_contain=["Example Domain"], tags=["baseline"]),
    Scenario("wikipedia", "https://en.wikipedia.org/wiki/Web_browser",
             min_text=1500, min_links=300, max_ms=20_000,
             must_contain=["web browser", "Function"], tags=["text", "links"]),
    Scenario("hackernews", "https://news.ycombinator.com/",
             min_text=300, min_links=200, max_ms=12_000, tags=["links", "dense"]),
    Scenario("github", "https://github.com/rust-lang/rust",
             min_text=200, min_links=50, max_ms=30_000, tags=["spa", "css"]),
    Scenario("youtube", "https://www.youtube.com/",
             min_text=50, min_links=20, max_ms=45_000, tags=["spa", "js"]),
    Scenario("duckduckgo", "https://lite.duckduckgo.com/lite/?q=rust+programming",
             min_text=100, min_links=20, max_ms=20_000, tags=["search"]),
    Scenario("habr", "https://habr.com/ru/",
             min_text=500, min_links=100, max_ms=25_000, tags=["text", "media"]),
    Scenario("stackoverflow", "https://stackoverflow.com/questions",
             min_text=200, min_links=50, max_ms=25_000, tags=["spa"]),
    Scenario("selfclick", "https://en.wikipedia.org/wiki/Web_browser",
             min_text=1500, min_links=300, max_ms=20_000,
             # клик по тексту ссылки, а не по заученной точке: раскладка меняется
             click_link="Application_software", click_expect="Application_software",
             tags=["interaction"]),
]


KEYS = ("page height", "text nodes", "fetched", "status", "scheme", "url", "title",
        "styles", "links", "note", "icon", "png", "element", "navigate", "form",
        "jump", "focus", "page")
KEY_RE = re.compile(r"^(?P<key>" + "|".join(k.replace(" ", r"\s") for k in KEYS) + r")\s+(?P<value>.+)$")
MS_RE = re.compile(r"(?P<ms>\d+)\s*ms")


def image_stats(path: Path) -> Optional[Dict[str, float]]:
    """Пиксельная правда: черный экран, пустая страница или «скелет»."""
    try:
        from PIL import Image  # type: ignore
    except Exception:  # noqa: BLE001 - без Pillow проверка просто пропускается
        return None
    if not path.exists():
        return None
    with Image.open(path) as img:
        img = img.convert("RGB")
        width, height = img.size
        pixels = list(img.getdata())
    if not pixels:
        return None
    counts: Dict[tuple, int] = {}
    for px in pixels:
        counts[px] = counts.get(px, 0) + 1
    background, bg_count = max(counts.items(), key=lambda kv: kv[1])
    def lum(px: tuple) -> float:
        return 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2]
    bg_lum = lum(background)
    ink = 0
    ink_lum = 0.0
    for px, count in counts.items():
        if abs(lum(px) - bg_lum) > 8:
            ink += count
            ink_lum += lum(px) * count
    gap = 0.0
    if ink:
        gap = abs(ink_lum / ink - bg_lum)
    return {
        "ink": ink / len(pixels),
        "colors": float(len(counts)),
        "bg_lum": bg_lum,
        "gap": gap,
        "w": float(width),
        "h": float(height),
    }


def parse(stdout: str) -> Dict[str, Any]:
    """Строки вида `links       939` в словарь."""
    out: Dict[str, Any] = {}
    for line in stdout.splitlines():
        m = KEY_RE.match(line.strip())
        if not m:
            continue
        key = m.group("key").strip().lower().replace(" ", "_")
        out[key] = m.group("value").strip()
    return out


def number(fields: Dict[str, Any], key: str) -> Optional[int]:
    value = fields.get(key)
    if value is None:
        return None
    m = re.search(r"\d+", str(value).replace(" ", ""))
    return int(m.group(0)) if m else None


def _run(binary: Path, args: List[str], timeout: float) -> tuple:
    started = time.monotonic()
    try:
        result = subprocess.run([str(binary)] + args, capture_output=True, text=True, timeout=timeout)
        return result.stdout, result.stderr, result.returncode, int((time.monotonic() - started) * 1000)
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout.decode(errors="replace") if isinstance(exc.stdout, bytes) else (exc.stdout or "")
        return stdout, f"timeout after {timeout:.0f}s", -1, int((time.monotonic() - started) * 1000)


def run_scenario(binary: Path, scenario: Scenario, outdir: Path) -> Dict[str, Any]:
    base = ["--url", scenario.url, "--width", str(scenario.width), "--height", str(scenario.height)]
    checks: List[Check] = []
    elapsed = 0
    fields: Dict[str, Any] = {}
    stdout = ""

    if scenario.click_link:
        listing, _, lcode, _ = _run(binary, base + ["--click", "-1,-1", "--text"],
                                    scenario.max_ms / 1000.0 + 30)
        boxes = re.findall(r"link (\d+),(\d+) (\d+)x(\d+) (\S+)", listing)
        target = next((b for b in boxes if scenario.click_link in b[4]), None)
        if target is None:
            checks.append(Check("клик", False, f"ссылка «{scenario.click_link}» не найдена среди {len(boxes)}", critical=True))
            elapsed = 0
            fields = {}
            stdout = listing
        else:
            x0, y0, w, h = (int(v) for v in target[:4])
            scenario.click = (x0 + w // 2, y0 + h // 2)
            checks.append(Check("координаты", True, f"центр ссылки {scenario.click} из {len(boxes)} ссылок"))

    if scenario.click:
        x, y = scenario.click
        stdout, stderr, code, elapsed = _run(binary, base + ["--click", f"{x},{y}", "--text",
                                                             "--timeout", str(scenario.max_ms / 1000.0 + 15)],
                                             scenario.max_ms / 1000.0 + 30)
        fields = parse(stdout)
        checks.append(Check("exit", code == 0, f"код {code}", critical=True))
        got = fields.get("navigate", "")
        if "Some(" in got:
            got = got.split("Some(", 1)[1].rstrip(")")
        checks.append(Check("клик", scenario.click_expect in got,
                            f"{got or 'никуда'} (ждём «{scenario.click_expect}»)", critical=True))
        (outdir / f"{scenario.site}.txt").write_text(stdout + "\n--- stderr ---\n" + stderr)
        passed = sum(1 for c in checks if c.ok)
        score = 100.0 * sum(c.weight for c in checks if c.ok) / sum(c.weight for c in checks)
        return {"site": scenario.site, "url": scenario.url, "tags": scenario.tags,
                "elapsed_ms": elapsed, "text": 0, "links": 0, "height": 0,
                "checks": [{"name": c.name, "ok": c.ok, "detail": c.detail} for c in checks],
                "passed": passed, "total": len(checks), "score": round(score, 1)}

    # 1) текстовая правда: узлы, ссылки, ожидаемые слова
    stdout, stderr, code, elapsed = _run(binary, base + ["--text", "--timeout", str(scenario.max_ms / 1000.0 + 15)],
                                         scenario.max_ms / 1000.0 + 30)
    fields = parse(stdout)
    (outdir / f"{scenario.site}.txt").write_text(stdout + "\n--- stderr ---\n" + stderr)
    checks.append(Check("exit", code == 0, f"код {code}", critical=True))
    if "status" in fields:
        checks.append(Check("status", fields.get("status") == "200", f"HTTP {fields.get('status', '?')}", critical=True))
    text = number(fields, "text_nodes") or 0
    links = number(fields, "links") or 0
    if "text nodes" in fields:
        checks.append(Check("текст", text >= scenario.min_text,
                            f"{text} узлов (нужно ≥{scenario.min_text})", critical=True))
    if "links" in fields:
        checks.append(Check("ссылки", links >= scenario.min_links,
                            f"{links} (нужно ≥{scenario.min_links})", critical=True))
    for needle in scenario.must_contain:
        found = needle.lower() in stdout.lower()
        checks.append(Check(f"текст «{needle}»", found, "найден" if found else "нет"))

    # 2) пиксельная правда
    png = outdir / f"{scenario.site}.png"
    _, err2, code2, ms2 = _run(binary, base + ["--dump", str(png)], scenario.max_ms / 1000.0 + 30)
    elapsed += ms2
    height = 0
    stats = image_stats(png)
    if stats is not None:
        height = int(stats["h"])
        checks.append(Check("визуал", stats["ink"] >= scenario.min_ink,
                            f"закрашено {stats['ink'] * 100:.2f}% (нужно ≥{scenario.min_ink * 100:.1f}%)", critical=True))
        checks.append(Check("цвета", stats["colors"] >= scenario.min_colors,
                            f"{stats['colors']:.0f} цветов (нужно ≥{scenario.min_colors})"))
        checks.append(Check("контраст", stats["gap"] >= 18 or stats["ink"] < scenario.min_ink,
                            f"разрыв яркости {stats['gap']:.0f}"))
    if code2 != 0:
        checks.append(Check("рендер без падений", "panicked" not in err2, err2.strip().splitlines()[-1][:100] if err2.strip() else "—"))

    checks.append(Check("время", elapsed <= scenario.max_ms, f"{elapsed} ms (бюджет {scenario.max_ms})"))

    passed = sum(1 for c in checks if c.ok)
    weight = sum(3.0 if c.critical else c.weight for c in checks)
    got_weight = sum(3.0 if c.critical else c.weight for c in checks if c.ok)
    score = 100.0 * got_weight / weight if weight else 0.0
    if any(c.critical and not c.ok for c in checks):
        score = min(score, 70.0)   # критический провал не может дать «хорошо»
    return {"site": scenario.site, "url": scenario.url, "tags": scenario.tags,
            "elapsed_ms": elapsed, "text": text, "links": links, "height": height,
            "checks": [{"name": c.name, "ok": c.ok, "detail": c.detail} for c in checks],
            "passed": passed, "total": len(checks), "score": round(score, 1)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--sites", nargs="*", default=[])
    parser.add_argument("--out", type=Path, default=Path("/tmp/sepia-qa"))
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    binary = args.binary.resolve()
    if not binary.exists():
        print(f"нет бинарника: {binary}", file=sys.stderr)
        return 2
    args.out.mkdir(parents=True, exist_ok=True)

    previous: Dict[str, float] = {}
    try:
        old = json.loads((args.out / "report.json").read_text())
        previous = {r["site"]: r["score"] for r in old["results"]}
    except Exception:  # noqa: BLE001 - первый прогон
        pass

    scenarios = [s for s in SCENARIOS if not args.sites or s.site in args.sites]
    results = []
    for scenario in scenarios:
        result = run_scenario(binary, scenario, args.out)
        results.append(result)
        if not args.json:
            mark = "OK  " if result["score"] >= 80 else "FAIL"
            print(f"[{mark}] {result['site']:<12} {result['score']:>5.1f}%  "
                  f"{result['elapsed_ms']:>6} ms  text={result['text']:<5} links={result['links']:<5}")
            for check in result["checks"]:
                if not check["ok"]:
                    print(f"         · {check['name']}: {check['detail']}")
            if result["site"] in previous:
                delta = result["score"] - previous[result["site"]]
                if abs(delta) >= 1.0:
                    print(f"         ~ к прошлому прогону: {delta:+.1f}%")

    previous = {}
    try:
        previous = {r["site"]: r["score"] for r in json.loads((args.out / "report.json").read_text())["results"]}
    except Exception:  # noqa: BLE001 - первый прогон
        pass

    total = sum(r["score"] for r in results) / len(results) if results else 0.0
    report = {"generated": time.strftime("%Y-%m-%d %H:%M"), "average": round(total, 1), "results": results}
    (args.out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2))
    else:
        print(f"\nсредняя оценка: {total:.1f}%  ({len(results)} сценариев, отчёт: {args.out / 'report.json'})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
